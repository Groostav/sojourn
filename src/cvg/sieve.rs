//! The GPU sieve: the brute squad's wide end.
//!
//! One compute shader per problem — every constraint rendered as a WGSL
//! function by [`crate::eval::wgsl`], plus a harness that draws candidates,
//! evaluates them in `f32`, and appends the ones within slack of feasible to
//! a survivor buffer. The CPU re-judges every survivor exactly, so the GPU
//! never decides an answer; it only makes a billion proposals cheap.
//!
//! # What is deterministic, and what is not
//!
//! Candidates are drawn on the device from a 32-bit hash of `(base, batch,
//! lane, dimension)` — WGSL has no `u64`, so the CPU's Xoshiro256++ stays on
//! the CPU. On one machine the same `(base, batch)` draws the same candidates
//! and keeps the same survivors, and survivors are sorted by lane before they
//! leave, so the atomic append order does not leak. Across machines the
//! special-function units round `sin` and `pow` differently, so the survivor
//! set — and therefore the seed brute force lands — may differ from one GPU
//! to another. The CPU path is the machine-independent reference;
//! [`super::ConstraintSolver::with_gpu`] turns this off for a run that must
//! reproduce anywhere.
//!
//! # Hangs
//!
//! Every wait on the device carries [`DISPATCH_TIMEOUT`]. A device that does
//! not answer in time is treated as lost: the call returns `None`, the
//! sampler drops the sieve and finishes the budget on the CPU.
//!
//! # Which device
//!
//! wgpu's high-performance preference, unless [`GPU_VARIABLE`] says
//! otherwise: `off`, an index, or a substring of the adapter's name. When it
//! is set the whole adapter list is logged at `info`, and a value that
//! matches nothing is a `warn` and the CPU path, so a wrong guess is visible.
//!
//! # How long the device is held
//!
//! Exactly as long as some brute-force search is using it. A [`Sieve`] holds
//! an `Arc` to the connection; the module keeps only a `Weak`, so a second
//! search overlapping the first shares the device and the last one out drops
//! it — instance, adapter, device and queue together. A library should not
//! keep a GPU open on a host's behalf after it has finished with it. The price
//! is reconnecting, on the order of a hundred milliseconds, per solve that
//! reaches brute force, on a path about to spend seconds anyway. "No adapter"
//! is remembered, so a machine without one is asked once per process.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use askama::Template;
use faer::MatRef;
use wgpu::util::DeviceExt;

use crate::eval::wgsl::{Function, Prelude};
use crate::solve::GpuTarget;
use crate::{ConstraintSystem, GPU_VARIABLE, Point};

/// Candidates per dispatch. Four million: sixteen thousand workgroups of
/// [`WORKGROUP`], well inside the 65,535 a dispatch dimension allows, and at a
/// billion-proposal budget about 240 dispatches.
pub(crate) const GPU_BATCH: u32 = 1 << 22;

/// Invocations per workgroup. A wavefront multiple on every vendor.
const WORKGROUP: u32 = 256;

/// Bytes of survivor buffer, which sets how many survivors one dispatch can
/// report: 65k for three variables, 5k for two hundred. An overflow loses
/// nothing that matters — the batch already holds more hits than a walker
/// needs — and the log line says it happened.
const SURVIVOR_BYTES: u64 = 4 << 20;

/// How long one dispatch may take before the device is given up on.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// A connection to the GPU: instance, adapter, device and queue, alive while
/// any [`Sieve`] holds it. See the module docs for why it is not kept longer.
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    /// Error scopes are a stack per *device*, not per thread. Two solves in
    /// flight at once — a dropped future's worker finishing its dispatch
    /// while the next solve starts — interleave their pushes and pops and
    /// wgpu reports a mismatched pop. One dispatch at a time is also the
    /// honest picture of one GPU.
    turn: Mutex<()>,
}

/// The live connection, if any search currently holds one.
static GPU: Mutex<Weak<Gpu>> = Mutex::new(Weak::new());

/// Set once a connection attempt found no adapter, so the next solve does not
/// pay to be told again.
static NO_ADAPTER: AtomicBool = AtomicBool::new(false);

/// The current connection, shared with whoever else holds it, or a fresh one.
fn acquire(state: &GpuTarget) -> Option<Arc<Gpu>> {
    if state == &GpuTarget::Off {
        return None;
    }
    if NO_ADAPTER.load(Ordering::Relaxed) && matches!(state, GpuTarget::Default) {
        return None;
    }

    let mut slot: MutexGuard<Weak<Gpu>> = GPU
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if let Some(live) = slot.upgrade() {
        let matches_live = match state {
            GpuTarget::Off => false,
            GpuTarget::Default => true,
            GpuTarget::Named(choice) => matches(
                choice,
                0,
                &live.info.name,
                &format!("{:?}", live.info.backend),
            ),
        };
        if matches_live {
            return Some(live);
        }
    }
    match connect(state) {
        Some(gpu) => {
            let gpu = Arc::new(gpu);
            *slot = Arc::downgrade(&gpu);
            Some(gpu)
        }
        None => {
            if matches!(state, GpuTarget::Default) {
                NO_ADAPTER.store(true, Ordering::Relaxed);
            }
            None
        }
    }
}

fn connect(state: &GpuTarget) -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = match state {
        GpuTarget::Off => return None,
        GpuTarget::Named(choice) => choose(&instance, choice)?,
        GpuTarget::Default => match std::env::var(GPU_VARIABLE) {
            Ok(choice) => choose(&instance, &choice)?,
            Err(_) => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                ..Default::default()
            }))
            .map_err(|error| tracing::info!(%error, "no GPU adapter; brute force stays on the CPU"))
            .ok()?,
        },
    };
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("babel brute squad"),
        ..Default::default()
    }))
    .map_err(|error| tracing::info!(%error, adapter = %info.name, "GPU device request failed"))
    .ok()?;
    // wgpu's default handler for an error nobody caught is a panic. A lost
    // device should read as "the sieve stopped", which
    // the timeout on every wait already turns it into.
    device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
        tracing::error!(%error, "uncaptured GPU error");
    }));
    tracing::info!(adapter = %info.name, backend = ?info.backend, "GPU sieve available");
    Some(Gpu {
        device,
        queue,
        info,
        turn: Mutex::new(()),
    })
}

/// The adapter [`GPU_VARIABLE`] asks for, out of everything wgpu can see.
fn choose(instance: &wgpu::Instance, choice: &str) -> Option<wgpu::Adapter> {
    if choice.eq_ignore_ascii_case("off") || choice.eq_ignore_ascii_case("none") {
        tracing::info!("{GPU_VARIABLE}={choice}: brute force stays on the CPU");
        return None;
    }
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY));
    let listed: Vec<String> = adapters
        .iter()
        .enumerate()
        .map(|(index, adapter)| {
            let info = adapter.get_info();
            format!(
                "{index}: {} ({:?}, {:?})",
                info.name, info.backend, info.device_type
            )
        })
        .collect();
    tracing::info!(choice, adapters = ?listed, "{GPU_VARIABLE} is set");

    let position = adapters.iter().enumerate().position(|(index, adapter)| {
        let info = adapter.get_info();
        matches(choice, index, &info.name, &format!("{:?}", info.backend))
    });
    match position {
        Some(index) => adapters.into_iter().nth(index),
        None => {
            tracing::warn!(
                choice,
                adapters = ?listed,
                "{GPU_VARIABLE} matched no adapter; brute force stays on the CPU"
            );
            None
        }
    }
}

/// Whether `choice` names the adapter at `index` called `name` on `backend`:
/// an index, a case-insensitive substring of the name, or the backend.
/// Pure, so it is tested without a device.
fn matches(choice: &str, index: usize, name: &str, backend: &str) -> bool {
    let choice = choice.trim();
    if let Ok(wanted) = choice.parse::<usize>() {
        return wanted == index;
    }
    let wanted = choice.to_lowercase();
    name.to_lowercase().contains(&wanted) || backend.to_lowercase() == wanted
}

/// The adapter's name, for the ledgers. `None` without one. Connects if
/// nothing else is holding the device, and lets go again on return.
pub(crate) fn adapter_name(state: &GpuTarget) -> Option<String> {
    acquire(state).map(|gpu| format!("{} ({:?})", gpu.info.name, gpu.info.backend))
}

/// The buffers the shader binds, by slot.
///
/// The one place the shader's `@binding` declarations, the bind-group layout
/// and the bind group agree: [`BINDINGS`] renders the declarations through
/// `harness.wgsl.jinja`, builds the layout entries, and names the buffer for each
/// slot through an exhaustive `match`. Adding a buffer is one row, one
/// variant, and the compiler names every place that must follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Params,
    Bounds,
    Survivors,
    Taken,
    Given,
}

/// How a slot is bound: a uniform, or storage the shader reads or writes.
#[derive(Debug, Clone, Copy)]
enum Access {
    Uniform,
    Storage { read_only: bool },
}

/// One row of the binding table.
pub(crate) struct Binding {
    slot: Slot,
    /// The WGSL identifier.
    pub(crate) name: &'static str,
    /// The WGSL address space and access mode, as written in `var<…>`.
    pub(crate) space: &'static str,
    /// The WGSL type.
    pub(crate) ty: &'static str,
    access: Access,
}

impl Binding {
    /// The `@binding` index: the slot's position in [`BINDINGS`].
    pub(crate) const fn index(&self) -> u32 {
        self.slot as u32
    }

    fn layout_entry(&self) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding: self.index(),
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: match self.access {
                    Access::Uniform => wgpu::BufferBindingType::Uniform,
                    Access::Storage { read_only } => wgpu::BufferBindingType::Storage { read_only },
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }
    }
}

const BINDINGS: [Binding; 5] = [
    Binding {
        slot: Slot::Params,
        name: "params",
        space: "uniform",
        ty: "Params",
        access: Access::Uniform,
    },
    Binding {
        slot: Slot::Bounds,
        name: "bounds",
        space: "storage, read",
        ty: "array<vec2<f32>>",
        access: Access::Storage { read_only: true },
    },
    Binding {
        slot: Slot::Survivors,
        name: "survivors",
        space: "storage, read_write",
        ty: "array<f32>",
        access: Access::Storage { read_only: false },
    },
    Binding {
        slot: Slot::Taken,
        name: "taken",
        space: "storage, read_write",
        ty: "atomic<u32>",
        access: Access::Storage { read_only: false },
    },
    Binding {
        slot: Slot::Given,
        name: "given",
        space: "storage, read",
        ty: "array<f32>",
        access: Access::Storage { read_only: true },
    },
];

/// The whole shader, in the shape `templates/wgsl/harness.wgsl.jinja` renders.
#[derive(Template)]
#[template(path = "wgsl/harness.wgsl.jinja", escape = "none")]
struct Shader {
    prelude: Prelude,
    functions: Vec<Function>,
    bindings: &'static [Binding],
    n: usize,
    workgroup: u32,
}

/// A compiled sieve for one problem.
pub(crate) struct Sieve {
    gpu: Arc<Gpu>,
    /// Shared by both pipelines. Explicit rather than derived from either
    /// entry point: each uses a different subset of the bindings, and a
    /// layout derived from one rejects a bind group meant for the other.
    layout: wgpu::BindGroupLayout,
    generated: wgpu::ComputePipeline,
    given: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    bounds: wgpu::Buffer,
    survivors: wgpu::Buffer,
    taken: wgpu::Buffer,
    /// Where `survivors` and `taken` are copied for reading: the two mapped
    /// regions, back to back.
    staging: wgpu::Buffer,
    /// A four-byte stand-in for the `given` binding when generating.
    no_candidates: wgpu::Buffer,
    variables: usize,
    capacity: u32,
}

impl Sieve {
    /// Compiles the problem's constraints into one shader. `None` when there
    /// is no adapter, or the shader does not build — the caller then takes
    /// the CPU path, which is never wrong, only slower.
    pub(crate) fn new(problem: &ConstraintSystem, state: &GpuTarget) -> Option<Self> {
        let gpu = acquire(state)?;
        let variables = problem.variables().len();
        if variables == 0 {
            return None;
        }
        let source = shader(problem);

        // A shader that does not compile must come back as `None`, not as a
        // panic. The error scope catches validation
        // failures from both the module and the pipelines built on it.
        let _turn = gpu
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("babel sieve"),
                source: wgpu::ShaderSource::Wgsl(source.as_str().into()),
            });
        let entries: Vec<wgpu::BindGroupLayoutEntry> =
            BINDINGS.iter().map(Binding::layout_entry).collect();
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sieve"),
                entries: &entries,
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sieve"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let pipeline = |entry: &str| {
            gpu.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                })
        };
        let generated = pipeline("sieve_generated");
        let given = pipeline("sieve_given");
        if let Some(error) = pollster::block_on(scope.pop()) {
            tracing::warn!(%error, "the sieve shader did not build; brute force stays on the CPU");
            tracing::debug!(shader = %source);
            return None;
        }
        drop(_turn);

        let row_bytes = u64::try_from((variables + 1) * 4).expect("a row fits");
        let capacity = u32::try_from((SURVIVOR_BYTES / row_bytes).max(1)).expect("fits u32");
        let survivor_bytes = u64::from(capacity) * row_bytes;

        let usage = wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST;
        let params = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bounds = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bounds"),
                contents: &bytes_of_f32(problem.variables.iter().flat_map(|variable| {
                    [narrow(variable.lower_bound), narrow(variable.upper_bound)]
                })),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let survivors = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("survivors"),
            size: survivor_bytes,
            usage,
            mapped_at_creation: false,
        });
        let taken = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("taken"),
            size: 4,
            usage,
            mapped_at_creation: false,
        });
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size: survivor_bytes + 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let no_candidates = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("no candidates"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        Some(Self {
            gpu,
            layout,
            generated,
            given,
            params,
            bounds,
            survivors,
            taken,
            staging,
            no_candidates,
            variables,
            capacity,
        })
    }

    /// Draws `count` candidates from `(base, batch)` on the device, sieves
    /// them, and returns the survivors as points, in lane order. `None` means
    /// the device failed or timed out; the caller should stop using it.
    pub(crate) fn sieve_generated(&self, base: u64, batch: u64, count: u32) -> Option<Vec<Point>> {
        let bind = self.bind_group(&self.no_candidates);
        let rows = self.run(&self.generated, &bind, base, batch, count)?;
        Some(
            rows.into_iter()
                .map(|(_, coordinates)| coordinates.into_iter().map(f64::from).collect())
                .collect(),
        )
    }

    /// Sieves candidates the caller supplies — one column each, as the
    /// evaluator takes them — and returns the indices of the columns that
    /// survived, ascending. For the tests and the throughput dial; the CPU
    /// keeps the `f64` originals to re-judge.
    pub(crate) fn sieve_given(&self, candidates: MatRef<'_, f64>) -> Option<Vec<usize>> {
        assert_eq!(candidates.nrows(), self.variables, "one row per variable");
        let count = u32::try_from(candidates.ncols()).ok()?;
        let flat = (0..candidates.ncols()).flat_map(|column| {
            (0..self.variables).map(move |row| narrow(candidates[(row, column)]))
        });
        let given = self
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("given"),
                contents: &bytes_of_f32(flat),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let bind = self.bind_group(&given);
        let rows = self.run(&self.given, &bind, 0, 0, count)?;
        Some(rows.into_iter().map(|(lane, _)| lane as usize).collect())
    }

    /// The buffer bound at `slot`; `given` is the caller's candidates for
    /// `sieve_given` and a stand-in when generating.
    fn buffer<'a>(&'a self, slot: Slot, given: &'a wgpu::Buffer) -> &'a wgpu::Buffer {
        match slot {
            Slot::Params => &self.params,
            Slot::Bounds => &self.bounds,
            Slot::Survivors => &self.survivors,
            Slot::Taken => &self.taken,
            Slot::Given => given,
        }
    }

    fn bind_group(&self, given: &wgpu::Buffer) -> wgpu::BindGroup {
        let entries: Vec<wgpu::BindGroupEntry<'_>> = BINDINGS
            .iter()
            .map(|binding| wgpu::BindGroupEntry {
                binding: binding.index(),
                resource: self.buffer(binding.slot, given).as_entire_binding(),
            })
            .collect();
        self.gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sieve"),
                layout: &self.layout,
                entries: &entries,
            })
    }

    /// One dispatch and its readback: `(lane, coordinates)` per survivor,
    /// sorted by lane.
    fn run(
        &self,
        pipeline: &wgpu::ComputePipeline,
        bind: &wgpu::BindGroup,
        base: u64,
        batch: u64,
        count: u32,
    ) -> Option<Vec<(u32, Vec<f32>)>> {
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;
        let _turn = self
            .gpu
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A validation error anywhere in the dispatch — a binding the layout
        // rejects, a copy out of range — would otherwise go to the uncaptured
        // handler and read as "no survivors". Here it reads as a failure.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let mut params = Vec::with_capacity(32);
        for word in [
            base as u32,
            (base >> 32) as u32,
            batch as u32,
            (batch >> 32) as u32,
            count,
            self.capacity,
            0,
            0,
        ] {
            params.extend_from_slice(&word.to_le_bytes());
        }
        queue.write_buffer(&self.params, 0, &params);
        queue.write_buffer(&self.taken, 0, &0u32.to_le_bytes());

        let row_bytes = u64::try_from((self.variables + 1) * 4).expect("a row fits");
        let survivor_bytes = u64::from(self.capacity) * row_bytes;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sieve"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sieve"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(count.div_ceil(WORKGROUP), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&self.taken, 0, &self.staging, 0, 4);
        encoder.copy_buffer_to_buffer(&self.survivors, 0, &self.staging, 4, survivor_bytes);
        queue.submit([encoder.finish()]);

        let (sender, receiver) = std::sync::mpsc::channel();
        self.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                drop(sender.send(result));
            });
        let polled = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(DISPATCH_TIMEOUT),
        });
        if let Err(error) = polled {
            tracing::error!(%error, "the GPU did not finish a dispatch in {DISPATCH_TIMEOUT:?}; giving it up");
            return None;
        }
        if let Some(error) = pollster::block_on(scope.pop()) {
            tracing::error!(%error, "the sieve dispatch was rejected");
            return None;
        }
        match receiver.try_recv() {
            Ok(Ok(())) => {}
            other => {
                tracing::error!(?other, "mapping the sieve's readback failed");
                return None;
            }
        }

        let rows = {
            let view = self.staging.slice(..).get_mapped_range().ok()?;
            let taken = u32::from_le_bytes(view[..4].try_into().expect("four bytes"));
            if taken > self.capacity {
                tracing::debug!(
                    taken,
                    capacity = self.capacity,
                    "survivor buffer overflowed; keeping the first"
                );
            }
            let reported = taken.min(self.capacity) as usize;
            let stride = (self.variables + 1) * 4;
            let mut rows: Vec<(u32, Vec<f32>)> = (0..reported)
                .map(|slot| {
                    let at = 4 + slot * stride;
                    let lane = u32::from_le_bytes(view[at..at + 4].try_into().expect("four bytes"));
                    let coordinates = (0..self.variables)
                        .map(|i| {
                            let from = at + 4 + i * 4;
                            f32::from_le_bytes(view[from..from + 4].try_into().expect("four bytes"))
                        })
                        .collect();
                    (lane, coordinates)
                })
                .collect();
            rows.sort_unstable_by_key(|(lane, _)| *lane);
            rows
        };
        self.staging.unmap();
        Some(rows)
    }
}

/// The whole shader for a problem: the emitter's prelude, one function per
/// constraint, and the harness.
/// The whole shader for a problem: the prelude, one function per constraint,
/// and the harness, rendered from `templates/wgsl/harness.wgsl.jinja`.
fn shader(problem: &ConstraintSystem) -> String {
    Shader {
        prelude: Prelude::new(),
        functions: problem
            .constraints
            .iter()
            .enumerate()
            .map(|(index, constraint)| constraint.compiled.wgsl(&format!("c{index}")))
            .collect(),
        bindings: &BINDINGS,
        n: problem.variables().len(),
        workgroup: WORKGROUP,
    }
    .render()
    .expect("the harness template renders for every problem that compiled")
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the sieve is f32 by design; precision loss is the documented trade"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

fn bytes_of_f32(values: impl Iterator<Item = f32>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    //! Every test here needs an adapter and says so when there is none, so
    //! CI without a GPU passes and `--nocapture` shows the skip.

    use faer::Mat;
    use rand::SeedableRng;
    use rand::rngs::Xoshiro256PlusPlus;

    use super::Sieve;
    use crate::cvg::sampling::fill_box;
    use crate::system::tests::system;
    use crate::{GpuTarget, InputVariable, Point};

    /// The table is in slot order and every name is distinct, which is what
    /// lets `@binding(index)` in the shader and the layout agree by
    /// construction.
    #[test]
    fn the_binding_table_is_in_slot_order() {
        for (position, binding) in super::BINDINGS.iter().enumerate() {
            assert_eq!(binding.index() as usize, position, "{}", binding.name);
        }
        let mut names: Vec<&str> = super::BINDINGS.iter().map(|b| b.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), super::BINDINGS.len());
    }

    #[test]
    fn the_adapter_choice_is_an_index_a_name_or_a_backend() {
        use super::matches;
        assert!(matches("1", 1, "AMD Radeon RX 7800 XT", "Vulkan"));
        assert!(!matches("1", 0, "AMD Radeon RX 7800 XT", "Vulkan"));
        // A bare number is an index, never a name: "7800" asks for adapter
        // 7800, not the 7800 XT. Name it with a letter in it.
        assert!(!matches("7800", 0, "AMD Radeon RX 7800 XT", "Vulkan"));
        assert!(matches("rx 7800", 0, "AMD Radeon RX 7800 XT", "Vulkan"));
        assert!(matches("radeon rx", 0, "AMD Radeon RX 7800 XT", "Vulkan"));
        assert!(matches("VULKAN", 0, "AMD Radeon RX 7800 XT", "Vulkan"));
        assert!(!matches("nvidia", 0, "AMD Radeon RX 7800 XT", "Vulkan"));
        assert!(
            !matches("vulk", 0, "AMD Radeon RX 7800 XT", "Vulkan"),
            "a backend is matched whole"
        );
    }

    macro_rules! sieve_or_skip {
        ($problem:expr) => {
            match Sieve::new(&$problem, &GpuTarget::Default) {
                Some(sieve) => sieve,
                None => {
                    eprintln!("no GPU adapter (or the shader did not build); test skipped");
                    return;
                }
            }
        };
    }

    fn unit_cube() -> Vec<InputVariable> {
        (1..=3)
            .map(|i| InputVariable::new(format!("x{i}"), 0.0, 1.0))
            .collect()
    }

    /// The three rung families at one in a thousand, a fault, an equality
    /// ribbon, and a transcendental mix.
    fn corpus() -> Vec<(&'static str, Vec<InputVariable>, Vec<&'static str>)> {
        vec![
            (
                "corner",
                unit_cube(),
                vec!["x1 > 0.9", "x2 > 0.9", "x3 > 0.9"],
            ),
            ("ball", unit_cube(), vec!["x1^2 + x2^2 + x3^2 < 0.0154"]),
            (
                "sine corner",
                unit_cube(),
                vec![
                    "sin(x1) > sin(0.9)",
                    "sin(x2) > sin(0.9)",
                    "sin(x3) > sin(0.9)",
                ],
            ),
            (
                "fault",
                vec![InputVariable::new("x1", 0.0, 10.0)],
                vec!["sqrt(x1 - 5) + x1 < 6"],
            ),
            ("ribbon", unit_cube(), vec!["x1 == x2 * x3 +/- 0.01"]),
            (
                "mix",
                unit_cube(),
                vec![
                    "ln(x1 + 0.5) < 0.2",
                    "log(x2 + 1, 10) > 0.1",
                    "cbrt(x3) < 0.9",
                    "x1 % 0.3 < 0.2",
                ],
            ),
        ]
    }

    fn candidates(inputs: &[InputVariable], count: usize, seed: u64) -> Mat<f64> {
        let bounds: Vec<(f64, f64)> = inputs
            .iter()
            .map(|i| (i.lower_bound, i.upper_bound))
            .collect();
        let mut matrix = Mat::zeros(inputs.len(), count);
        fill_box(
            &mut matrix,
            &bounds,
            &mut Xoshiro256PlusPlus::seed_from_u64(seed),
        );
        matrix
    }

    /// The sieve may keep too much, never too little: every column the CPU
    /// judges feasible in `f64` survives the `f32` pass.
    #[test]
    fn the_sieve_never_drops_a_feasible_point() {
        for (name, inputs, sources) in corpus() {
            let problem = system(inputs.clone(), &sources);
            let sieve = sieve_or_skip!(problem);
            let batch = candidates(&inputs, 100_000, 7);

            let survivors = sieve
                .sieve_given(batch.as_ref())
                .expect("the device answered");
            let exact: Vec<usize> = (0..batch.ncols())
                .filter(|&column| {
                    let point: Point = (0..inputs.len()).map(|row| batch[(row, column)]).collect();
                    problem.is_feasible(&point, 0.0)
                })
                .collect();

            assert!(
                !exact.is_empty(),
                "{name}: the fixture has no feasible column"
            );
            let survived: std::collections::HashSet<usize> = survivors.iter().copied().collect();
            let dropped: Vec<usize> = exact
                .iter()
                .copied()
                .filter(|column| !survived.contains(column))
                .collect();
            assert!(
                dropped.is_empty(),
                "{name}: the sieve dropped {} of {} feasible columns, e.g. column {:?}",
                dropped.len(),
                exact.len(),
                dropped.first()
            );
            assert!(
                survivors.len() <= 2 * exact.len() + 64,
                "{name}: {} survivors for {} feasible columns; the slack is too loose",
                survivors.len(),
                exact.len()
            );
        }
    }

    /// Nothing the CPU would fault on survives: below five, `sqrt` is NaN.
    #[test]
    fn a_faulting_candidate_does_not_survive() {
        let inputs = vec![InputVariable::new("x1", 0.0, 10.0)];
        let problem = system(inputs.clone(), &["sqrt(x1 - 5) + x1 < 6"]);
        let sieve = sieve_or_skip!(problem);
        let batch = candidates(&inputs, 10_000, 3);
        let survivors = sieve
            .sieve_given(batch.as_ref())
            .expect("the device answered");
        assert!(!survivors.is_empty());
        for column in survivors {
            assert!(batch[(0, column)] >= 4.99, "{}", batch[(0, column)]);
        }
    }

    /// A subscript out of range is a fault on both sides of the sieve.
    #[test]
    fn an_out_of_range_gather_does_not_survive() {
        let inputs = vec![
            InputVariable::new("x1", 0.0, 5.0),
            InputVariable::new("x2", 0.0, 1.0),
        ];
        // `var[ceil(x1)]`: valid for x1 in (0, 2], out of range above.
        let problem = system(inputs.clone(), &["var[ceil(x1)] > -1"]);
        let sieve = sieve_or_skip!(problem);
        let batch = candidates(&inputs, 10_000, 5);
        let survivors = sieve
            .sieve_given(batch.as_ref())
            .expect("the device answered");
        assert!(!survivors.is_empty());
        for column in survivors {
            assert!(batch[(0, column)] <= 2.0, "{}", batch[(0, column)]);
        }
    }

    /// Same `(base, batch)`, same survivors, in the same order; a different
    /// batch number is a different draw.
    #[test]
    fn generated_survivors_are_a_function_of_the_seed() {
        let problem = system(unit_cube(), &["x1 > 0.99", "x2 > 0.9"]);
        let sieve = sieve_or_skip!(problem);
        let once = sieve
            .sieve_generated(42, 7, 1 << 18)
            .expect("the device answered");
        let again = sieve
            .sieve_generated(42, 7, 1 << 18)
            .expect("the device answered");
        let other = sieve
            .sieve_generated(42, 8, 1 << 18)
            .expect("the device answered");
        assert!(
            !once.is_empty(),
            "a percent of a quarter million should land"
        );
        assert_eq!(once, again);
        assert_ne!(once, other);
        for point in &once {
            assert!(point[0] > 0.99 - 1e-3 && point[1] > 0.9 - 1e-3, "{point:?}");
        }
    }
}
