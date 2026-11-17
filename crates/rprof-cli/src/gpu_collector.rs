//! Device-wide NVIDIA gauges, independent of CUPTI and application CUDA visibility.
use crate::capture_sink::CaptureSink;
use anyhow::{Context, Result, ensure};
use rprof_core::{
    CounterSample, CounterValue, TrackKind,
    stream::{Attribute, Event, Payload},
};
use std::{
    collections::HashSet,
    ffi::{CStr, c_char, c_uint, c_void},
    fs,
    sync::Mutex,
};
use uuid::Uuid;

type Handle = *mut c_void;
type Status = c_uint;
type Init = unsafe extern "C" fn() -> Status;
type ErrorString = unsafe extern "C" fn(Status) -> *const c_char;
type Count = unsafe extern "C" fn(*mut c_uint) -> Status;
type DeviceAt = unsafe extern "C" fn(c_uint, *mut Handle) -> Status;
type Text = unsafe extern "C" fn(Handle, *mut c_char, c_uint) -> Status;
type ReadUtilization = unsafe extern "C" fn(Handle, *mut Utilization) -> Status;
type ReadMemory = unsafe extern "C" fn(Handle, *mut Memory) -> Status;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

struct Library(Handle);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.0) };
    }
}

struct Nvml {
    _library: Library,
    shutdown: Init,
    error_string: ErrorString,
    utilization: ReadUtilization,
    memory: ReadMemory,
}
impl Nvml {
    fn check(&self, status: Status, operation: &str) -> Result<()> {
        if status != 0 {
            let message = unsafe { (self.error_string)(status) };
            let detail = if message.is_null() {
                format!("NVML status {status}")
            } else {
                unsafe { CStr::from_ptr(message) }
                    .to_string_lossy()
                    .into_owned()
            };
            anyhow::bail!("{operation}: {detail} ({status})");
        }
        Ok(())
    }
    fn utilization(&self, device: Handle) -> Result<u32> {
        let mut value = Utilization::default();
        self.check(
            unsafe { (self.utilization)(device, &mut value) },
            "nvmlDeviceGetUtilizationRates",
        )?;
        ensure!(value.gpu <= 100, "NVML GPU utilization exceeds 100%");
        Ok(value.gpu)
    }
    fn memory(&self, device: Handle) -> Result<Memory> {
        let mut value = Memory::default();
        self.check(
            unsafe { (self.memory)(device, &mut value) },
            "nvmlDeviceGetMemoryInfo",
        )?;
        ensure!(
            value.total > 0 && value.used <= value.total && value.free <= value.total,
            "inconsistent NVML memory values"
        );
        Ok(value)
    }
}
impl Drop for Nvml {
    fn drop(&mut self) {
        if let Err(error) = self.check(unsafe { (self.shutdown)() }, "nvmlShutdown") {
            eprintln!("GPU monitoring shutdown: {error:#}");
        }
    }
}

struct Gpu {
    handle: Handle,
    index: u32,
    name: String,
    uuid: String,
}

pub(crate) struct Prepared {
    nvml: Nvml,
    devices: Vec<Gpu>,
}
impl Prepared {
    pub fn new() -> Result<Self> {
        // Resolve the stable NVML C ABI at runtime; CPU-only builds need no SDK or driver.
        let handle = unsafe {
            libc::dlopen(
                c"libnvidia-ml.so.1".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        if handle.is_null() {
            let error = unsafe { libc::dlerror() };
            let detail = if error.is_null() {
                "dynamic loader returned no diagnostic".into()
            } else {
                unsafe { CStr::from_ptr(error) }.to_string_lossy()
            };
            anyhow::bail!("--collect gpu requires NVIDIA NVML (libnvidia-ml.so.1): {detail}");
        }
        let library = Library(handle);
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                let pointer = unsafe { libc::dlsym(library.0, $name.as_ptr()) };
                ensure!(
                    !pointer.is_null(),
                    "NVML symbol {} unavailable",
                    $name.to_string_lossy()
                );
                unsafe { std::mem::transmute::<Handle, $ty>(pointer) }
            }};
        }
        let init = symbol!(c"nvmlInit_v2", Init);
        let shutdown = symbol!(c"nvmlShutdown", Init);
        let error_string = symbol!(c"nvmlErrorString", ErrorString);
        let count = symbol!(c"nvmlDeviceGetCount_v2", Count);
        let device_at = symbol!(c"nvmlDeviceGetHandleByIndex_v2", DeviceAt);
        let name = symbol!(c"nvmlDeviceGetName", Text);
        let uuid = symbol!(c"nvmlDeviceGetUUID", Text);
        let utilization = symbol!(c"nvmlDeviceGetUtilizationRates", ReadUtilization);
        let memory = symbol!(c"nvmlDeviceGetMemoryInfo", ReadMemory);
        let status = unsafe { init() };
        ensure!(
            status == 0,
            "nvmlInit_v2 failed with status {status}; check NVIDIA driver access"
        );
        let nvml = Nvml {
            _library: library,
            shutdown,
            error_string,
            utilization,
            memory,
        };
        let mut size = 0;
        nvml.check(unsafe { count(&mut size) }, "nvmlDeviceGetCount_v2")?;
        ensure!(
            (1..=256).contains(&size),
            "NVML reports {size} GPUs; expected 1..=256"
        );
        let mut devices = Vec::new();
        let mut identities = HashSet::new();
        for index in 0..size {
            let mut handle = std::ptr::null_mut();
            nvml.check(
                unsafe { device_at(index, &mut handle) },
                "nvmlDeviceGetHandleByIndex_v2",
            )?;
            ensure!(!handle.is_null(), "NVML returned a null GPU handle");
            let text = |function: Text, operation: &str| -> Result<String> {
                let mut buffer = [0u8; 96];
                nvml.check(
                    unsafe { function(handle, buffer.as_mut_ptr().cast(), buffer.len() as u32) },
                    operation,
                )?;
                Ok(CStr::from_bytes_until_nul(&buffer)
                    .context("unterminated NVML device metadata")?
                    .to_string_lossy()
                    .into_owned())
            };
            let name = text(name, "nvmlDeviceGetName")?;
            let uuid = text(uuid, "nvmlDeviceGetUUID")?;
            ensure!(
                !uuid.is_empty() && identities.insert(uuid.clone()),
                "missing or duplicate NVML GPU UUID"
            );
            devices.push(Gpu {
                handle,
                index,
                name,
                uuid,
            });
        }
        Ok(Self { nvml, devices })
    }
}

pub(crate) struct Running {
    source: Prepared,
    tracks: Vec<(u64, u64)>,
    utilization_track: u64,
    memory_track: u64,
    interval_ns: u64,
    last: Option<u64>,
    sample_index: u64,
    errors: u64,
    last_error: String,
    _lease: fs::File,
}
impl Running {
    pub fn start(
        source: Prepared,
        out: &mut CaptureSink,
        now: u64,
        interval_ms: u64,
        session: Uuid,
    ) -> Result<Option<Self>> {
        let host_id = format!(
            "{}/{}",
            fs::read_to_string("/proc/sys/kernel/hostname")?.trim(),
            fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim()
        );
        let Some(lease) =
            crate::system_collector::lease(&std::env::temp_dir(), session, &host_id, "gpu")?
        else {
            let mut event = Event::new(
                now,
                Payload::Counter(CounterSample {
                    track_id: 0,
                    timestamp_ns: now,
                    name: "system.meta.gpu.delegated".into(),
                    value: CounterValue::Integer(1),
                    unit: "status".into(),
                }),
            );
            event
                .attributes
                .insert("system.host_id".into(), Attribute::Text(host_id));
            event.attributes.insert("system.detail".into(), Attribute::Text("another rank in this session owns device-wide GPU sampling; retain its capture".into()));
            out.reliable(event)?;
            return Ok(None);
        };
        let mut track = |parent, name, resource| {
            out.track_with_parent(
                now,
                parent,
                name,
                TrackKind::System {
                    resource,
                    host_id: host_id.clone(),
                },
                None,
            )
        };
        let utilization_track = track(
            None,
            "GPU utilization · device average".into(),
            "gpu".into(),
        )?;
        let memory_track = track(
            None,
            "GPU memory used · device total".into(),
            "gpu_memory".into(),
        )?;
        let mut tracks = Vec::new();
        for device in &source.devices {
            let label = format!("GPU {} · {}", device.index, device.name);
            tracks.push((
                track(
                    Some(utilization_track),
                    label.clone(),
                    format!("gpu:{}", device.uuid),
                )?,
                track(
                    Some(memory_track),
                    label,
                    format!("gpu_memory:{}", device.uuid),
                )?,
            ));
        }
        Ok(Some(Self {
            source,
            tracks,
            utilization_track,
            memory_track,
            interval_ns: interval_ms * 1_000_000,
            last: None,
            sample_index: 0,
            errors: 0,
            last_error: String::new(),
            _lease: lease,
        }))
    }

    fn event(&self, now: u64, track: u64, name: &str, value: CounterValue, unit: &str) -> Event {
        let mut event = Event::new(
            now,
            Payload::Counter(CounterSample {
                track_id: track,
                timestamp_ns: now,
                name: name.into(),
                value,
                unit: unit.into(),
            }),
        );
        event.attributes.insert("system.scope".into(), Attribute::Text("NVML device-wide; includes other processes; not application-only or CUDA_VISIBLE_DEVICES filtered".into()));
        event.attributes.insert(
            "system.interval_ns".into(),
            Attribute::Unsigned(self.interval_ns),
        );
        event.attributes.insert(
            "system.sample_index".into(),
            Attribute::Unsigned(self.sample_index),
        );
        event
    }
    fn error(&mut self, index: usize, error: anyhow::Error) {
        self.errors = self.errors.saturating_add(1);
        self.last_error = format!("GPU {}: {error:#}", self.source.devices[index].index)
            .chars()
            .take(512)
            .collect();
    }
    fn emit_utilization(
        &self,
        out: &mut CaptureSink,
        now: u64,
        track: u64,
        value: f64,
        count: u64,
    ) -> Result<()> {
        let mut event = self.event(
            now,
            track,
            "system.gpu.utilization",
            CounterValue::Float(value),
            "%",
        );
        event.attributes.insert("system.definition".into(), Attribute::Text("NVML percent of driver sampling window with a kernel executing; not SM occupancy; driver window duration unknown".into()));
        event
            .attributes
            .insert("system.gpu.count".into(), Attribute::Unsigned(count));
        out.emit(event)
    }
    fn emit_memory(
        &self,
        out: &mut CaptureSink,
        now: u64,
        track: u64,
        value: Memory,
    ) -> Result<()> {
        let mut event = self.event(
            now,
            track,
            "system.gpu.memory.used",
            CounterValue::Integer(value.used),
            "bytes",
        );
        event.attributes.insert("system.definition".into(), Attribute::Text("NVML used device memory; includes driver/system allocations; not framework allocated/reserved bytes".into()));
        event.attributes.insert(
            "system.memory.total_bytes".into(),
            Attribute::Unsigned(value.total),
        );
        event.attributes.insert(
            "system.memory.free_bytes".into(),
            Attribute::Unsigned(value.free),
        );
        out.emit(event)
    }
    pub fn sample(&mut self, sink: &Mutex<CaptureSink>, now: u64, force: bool) -> Result<()> {
        if !force
            && self
                .last
                .is_some_and(|last| now.saturating_sub(last) < self.interval_ns)
        {
            return Ok(());
        }
        self.last = Some(now);
        self.sample_index += 1;
        let mut utilization = Vec::new();
        let mut memory = Vec::new();
        for index in 0..self.source.devices.len() {
            let handle = self.source.devices[index].handle;
            match self.source.nvml.utilization(handle) {
                Ok(value) => {
                    utilization.push((index, value as u64));
                }
                Err(error) => self.error(index, error),
            }
            match self.source.nvml.memory(handle) {
                Ok(value) => {
                    memory.push((index, value));
                }
                Err(error) => self.error(index, error),
            }
        }
        // Driver queries can block; never hold up native event receivers while polling NVML.
        let mut out = sink
            .lock()
            .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
        for &(index, value) in &utilization {
            self.emit_utilization(&mut out, now, self.tracks[index].0, value as f64, 1)?;
        }
        for &(index, value) in &memory {
            self.emit_memory(&mut out, now, self.tracks[index].1, value)?;
        }
        let count = self.source.devices.len();
        // A missing device read is unknown, not an idle GPU or a smaller denominator.
        if utilization.len() == count {
            self.emit_utilization(
                &mut out,
                now,
                self.utilization_track,
                utilization.iter().map(|(_, value)| value).sum::<u64>() as f64 / count as f64,
                count as u64,
            )?;
        }
        if memory.len() == count {
            let mut total = Memory::default();
            for (_, value) in memory {
                total.total = total
                    .total
                    .checked_add(value.total)
                    .context("GPU memory capacity overflow")?;
                total.free = total
                    .free
                    .checked_add(value.free)
                    .context("GPU free memory overflow")?;
                total.used = total
                    .used
                    .checked_add(value.used)
                    .context("GPU used memory overflow")?;
            }
            self.emit_memory(&mut out, now, self.memory_track, total)?;
        }
        let mut event = self.event(
            now,
            self.utilization_track,
            "system.meta.gpu.read_errors",
            CounterValue::Integer(self.errors),
            "count",
        );
        event.attributes.insert(
            "system.last_error".into(),
            Attribute::Text(self.last_error.clone()),
        );
        out.reliable(event)
    }
    pub fn finish(mut self, sink: &Mutex<CaptureSink>, now: u64) -> Result<()> {
        self.sample(sink, now, true)?;
        eprintln!(
            "GPU (NVML): {} devices, {} polling rounds, {} read errors{}",
            self.source.devices.len(),
            self.sample_index,
            self.errors,
            if self.errors > 0 {
                format!("; missing samples are unknown; {}", self.last_error)
            } else {
                String::new()
            }
        );
        Ok(())
    }
}
