use anyhow::{Context, Result, ensure};
use object::{Object, ObjectSegment, ObjectSymbol};
use rprof_core::stream::{FrameLanguage, StackFrame};
type PythonSymbols = Vec<(u64, u64, String)>;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek},
    os::unix::fs::MetadataExt,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub(crate) struct Mapping {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub path: String,
    pub device: u64,
    pub inode: u64,
}
#[derive(Clone, Default)]
pub(crate) struct Image {
    pub epoch: u64,
    pub name: String,
    pub mappings: Vec<Mapping>,
}
struct Symbol {
    address: u64,
    size: u64,
    name: String,
}
struct Module {
    identity: String,
    segments: Vec<(u64, u64, u64)>,
    symbols: Vec<Symbol>,
}
#[derive(Default)]
pub(crate) struct Symbols {
    namespace: String,
    pub images: HashMap<u32, Image>,
    modules: HashMap<String, Option<Module>>,
    native: HashMap<(u32, u64), Vec<Mapping>>,
    native_epochs: HashMap<(u32, u64), u64>,
    python: HashMap<(u32, u64), PythonSymbols>,
    python_files: HashMap<(u32, u64), Arc<File>>,
    pending_python: Vec<(u32, u64, u64)>,
    perf_time: u64,
    python_bytes: usize,
    bytes: usize,
    pub unresolved: u64,
    pub failures: u64,
    pub last_error: String,
}
impl Symbols {
    pub fn new() -> Self {
        Self {
            namespace: uuid::Uuid::new_v4().to_string(),
            ..Self::default()
        }
    }
    pub fn pin_python(&mut self, pid: u32, instance: u64, at: u64, file: Result<File>) {
        match file {
            Ok(file) if self.python_files.len() < 256 => {
                let file = Arc::new(file);
                self.python_files.insert((pid, instance), file);
                self.pending_python.push((pid, instance, at));
                self.advance_python(self.perf_time);
            }
            Ok(_) => {
                self.failures += 1;
                self.last_error = "Python symbol file limit exceeded".into();
            }
            Err(error) => {
                self.failures += 1;
                self.last_error = format!("Python symbols: {error:#}");
            }
        }
    }

    pub fn advance_python(&mut self, time: u64) {
        self.perf_time = self.perf_time.max(time);
        let mut pending = Vec::new();
        for (pid, instance, at) in self.pending_python.drain(..) {
            if at > self.perf_time {
                pending.push((pid, instance, at));
                continue;
            }
            if let Some(image) = self.images.get(&pid).filter(|i| i.epoch <= at) {
                if let Some(file) = self.python_files.get(&(pid, instance)).cloned() {
                    self.python_files.insert((pid, image.epoch), file);
                    self.native_epochs.insert((pid, instance), image.epoch);
                }
            }
        }
        self.pending_python = pending;
    }
    pub fn mapping(&mut self, pid: u32, mapping: Mapping) -> Result<()> {
        ensure!(
            self.images.contains_key(&pid) || self.images.len() < 4096,
            "stack process limit exceeded"
        );
        let image = self.images.entry(pid).or_default();
        if image
            .mappings
            .iter()
            .any(|m| m.start == mapping.start && m.end == mapping.end && m.path == mapping.path)
        {
            return Ok(());
        }
        image
            .mappings
            .retain(|m| m.end <= mapping.start || m.start >= mapping.end);
        ensure!(
            image.mappings.len() < 4096,
            "stack executable mapping limit exceeded"
        );
        image.mappings.push(mapping);
        Ok(())
    }

    pub fn resolve(&mut self, pid: u32, epoch: u64, ips: &[u64], native: bool) -> Vec<StackFrame> {
        let mappings = if native {
            let key = (pid, epoch);
            let reload = !self.native.contains_key(&key)
                || ips.iter().any(|ip| {
                    !self.native[&key]
                        .iter()
                        .any(|m| *ip > m.start && *ip <= m.end)
                });
            if reload && self.native.len() < 4096 {
                match live_instance(pid).and_then(|id| {
                    ensure!(
                        id == epoch,
                        "native process image changed before stack resolution"
                    );
                    process_mappings(pid)
                }) {
                    Ok(maps) => {
                        self.native.insert(key, maps);
                    }
                    Err(error) => {
                        self.failures = self.failures.saturating_add(1);
                        self.last_error = format!("{error:#}");
                    }
                }
            }
            let mut maps = self.native.get(&key).cloned().unwrap_or_default();
            if let Some(image) = self
                .images
                .get(&pid)
                .filter(|image| self.native_epochs.get(&key) == Some(&image.epoch))
            {
                for mapping in &image.mappings {
                    maps.retain(|m| m.end <= mapping.start || m.start >= mapping.end);
                    maps.push(mapping.clone());
                }
            }
            maps
        } else {
            self.images
                .get(&pid)
                .map(|i| i.mappings.clone())
                .unwrap_or_default()
        };
        let key = (pid, epoch);
        let needs_python = ips.iter().any(|pc| {
            !mappings
                .iter()
                .any(|m| *pc > m.start && *pc <= m.end && m.inode != 0)
                && !self.python.get(&key).is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|(start, end, _)| *pc > *start && *pc <= *end)
                })
        });
        if needs_python && self.python.len() < 4096 {
            let file = self
                .python_files
                .get(&key)
                .cloned()
                .map(Ok)
                .unwrap_or_else(|| {
                    File::open(format!("/proc/{pid}/root/tmp/perf-{pid}.map")).map(Arc::new)
                });
            match file.and_then(|file| python_map(&file)) {
                Ok(entries) => {
                    let cost = entries
                        .iter()
                        .map(|(_, _, name)| name.len() + 48)
                        .sum::<usize>();
                    let old = self
                        .python
                        .get(&key)
                        .map(|entries| {
                            entries
                                .iter()
                                .map(|(_, _, name)| name.len() + 48)
                                .sum::<usize>()
                        })
                        .unwrap_or(0);
                    if self.python_bytes - old + cost <= 32 * 1024 * 1024 {
                        self.python_bytes = self.python_bytes - old + cost;
                        self.python.insert(key, entries);
                    } else {
                        self.failures += 1;
                        self.last_error = "Python symbol cache exceeds 32 MiB".into();
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => {
                    self.failures = self.failures.saturating_add(1);
                    self.last_error = error.to_string();
                }
            }
        }
        ips.iter()
            .rev()
            .enumerate()
            .map(|(index, &address)| {
                let pc = if !native && index == ips.len() - 1 {
                    address
                } else {
                    address.saturating_sub(1)
                };
                if let Some((start, _, name)) = self.python.get(&key).and_then(|entries| {
                    entries
                        .iter()
                        .find(|(start, end, _)| pc >= *start && pc < *end)
                }) {
                    return StackFrame {
                        key: format!("python:{}:{pid}:{epoch}:{start}", self.namespace),
                        name: name.strip_prefix("py::").unwrap_or(name).to_owned(),
                        module: "CPython".into(),
                        language: FrameLanguage::Python,
                        address,
                    };
                }
                let mapping = mappings.iter().find(|m| pc >= m.start && pc < m.end);
                if let Some(mapping) = mapping {
                    let module_key =
                        format!("{}:{}:{}", mapping.path, mapping.device, mapping.inode);
                    if !self.modules.contains_key(&module_key)
                        && self.modules.len() < 1024
                        && mapping.inode != 0
                    {
                        match module(pid, mapping, &mut self.bytes, &self.namespace) {
                            Ok(module) => {
                                self.modules.insert(module_key.clone(), Some(module));
                            }
                            Err(error) => {
                                self.failures = self.failures.saturating_add(1);
                                self.last_error = format!("{}: {error:#}", mapping.path);
                                self.modules.insert(module_key.clone(), None);
                            }
                        }
                    }
                    if let Some(module) = self.modules.get(&module_key).and_then(Option::as_ref) {
                        let file_offset = pc - mapping.start + mapping.offset;
                        if let Some((offset, size, virtual_address)) =
                            module.segments.iter().find(|(offset, size, _)| {
                                file_offset >= *offset && file_offset - *offset < *size
                            })
                        {
                            let _ = size;
                            let symbol_address = virtual_address + file_offset - offset;
                            let index = module
                                .symbols
                                .partition_point(|s| s.address <= symbol_address);
                            if let Some(symbol) = index
                                .checked_sub(1)
                                .and_then(|i| module.symbols.get(i))
                                .filter(|s| symbol_address - s.address < s.size.max(1))
                            {
                                return StackFrame {
                                    key: format!("{}:{:x}", module.identity, symbol.address),
                                    name: symbol.name.clone(),
                                    module: mapping.path.clone(),
                                    language: FrameLanguage::Native,
                                    address,
                                };
                            }
                        }
                    }
                }
                self.unresolved = self.unresolved.saturating_add(1);
                StackFrame {
                    key: format!("unknown:{}:{pid}:{epoch}:{pc:x}", self.namespace),
                    name: format!("[unknown] 0x{pc:x}"),
                    module: mapping.map(|m| m.path.clone()).unwrap_or_default(),
                    language: FrameLanguage::Unknown,
                    address,
                }
            })
            .collect()
    }
}

fn module(pid: u32, mapping: &Mapping, used: &mut usize, namespace: &str) -> Result<Module> {
    let path = &mapping.path;
    ensure!(
        path.starts_with('/') && !path.ends_with(" (deleted)"),
        "module file unavailable"
    );
    let file = File::open(format!("/proc/{pid}/root{path}")).or_else(|_| File::open(path))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.ino() == mapping.inode && metadata.dev() == mapping.device,
        "module identity does not match the recorded mapping"
    );
    let cache = object::read::ReadCache::new(file);
    let object = object::File::parse(&cache)?;
    let identity = if let Some(build) = object.build_id()? {
        build.iter().map(|b| format!("{b:02x}")).collect::<String>()
    } else {
        format!(
            "file:{namespace}:{path}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime()
        )
    };
    let segments = object
        .segments()
        .map(|s| {
            let (offset, size) = s.file_range();
            (offset, size, s.address())
        })
        .collect();
    let mut symbols = Vec::new();
    for symbol in object.symbols().chain(object.dynamic_symbols()) {
        if symbol.kind() != object::SymbolKind::Text || !symbol.is_definition() {
            continue;
        }
        let Ok(name) = symbol.name() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        ensure!(
            symbols.len() < 262_144 && *used < 64 * 1024 * 1024,
            "stack symbol cache limit exceeded"
        );
        let name = if let Ok(symbol) = rustc_demangle::try_demangle(name) {
            format!("{symbol:#}")
        } else if let Ok(symbol) = cpp_demangle::Symbol::new(name) {
            symbol
                .demangle(&Default::default())
                .unwrap_or_else(|_| name.to_owned())
        } else {
            name.to_owned()
        };
        let name: String = name.chars().take(1024).collect();
        *used += name.len() + std::mem::size_of::<Symbol>();
        symbols.push(Symbol {
            address: symbol.address(),
            size: symbol.size(),
            name,
        });
    }
    symbols.sort_by(|a, b| {
        a.address
            .cmp(&b.address)
            .then(a.size.cmp(&b.size))
            .then(a.name.cmp(&b.name))
    });
    symbols.dedup_by(|a, b| a.address == b.address && a.name == b.name && a.size == b.size);
    Ok(Module {
        identity,
        segments,
        symbols,
    })
}

fn process_mappings(pid: u32) -> Result<Vec<Mapping>> {
    let mut maps = Vec::new();
    for line in BufReader::new(File::open(format!("/proc/{pid}/maps"))?)
        .lines()
        .take(16_384)
    {
        let line = line?;
        let mut fields = line.split_whitespace();
        let (Some(range), Some(permissions), Some(offset), Some(device), Some(inode)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        if !permissions.contains('x') {
            continue;
        }
        let (start, end) = range
            .split_once('-')
            .context("invalid executable mapping")?;
        let (major, minor) = device
            .split_once(':')
            .context("invalid executable mapping device")?;
        let path = fields.collect::<Vec<_>>().join(" ");
        ensure!(maps.len() < 4096, "executable mapping limit exceeded");
        maps.push(Mapping {
            start: u64::from_str_radix(start, 16)?,
            end: u64::from_str_radix(end, 16)?,
            offset: u64::from_str_radix(offset, 16)?,
            path,
            inode: inode.parse()?,
            device: libc::makedev(
                u32::from_str_radix(major, 16)?,
                u32::from_str_radix(minor, 16)?,
            ),
        });
    }

    Ok(maps)
}

fn live_instance(pid: u32) -> Result<u64> {
    use sha2::{Digest, Sha256};
    let mut auxv = Vec::new();
    File::open(format!("/proc/{pid}/auxv"))?
        .take(16_384)
        .read_to_end(&mut auxv)?;
    let address = auxv
        .chunks_exact(16)
        .find_map(|pair| {
            (u64::from_ne_bytes(pair[..8].try_into().unwrap()) == libc::AT_RANDOM)
                .then(|| u64::from_ne_bytes(pair[8..].try_into().unwrap()))
        })
        .context("process image identity unavailable")?;
    let mut material = [0u8; 20];
    let local = libc::iovec {
        iov_base: material.as_mut_ptr().cast(),
        iov_len: 16,
    };
    let remote = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: 16,
    };
    ensure!(
        unsafe { libc::process_vm_readv(pid as i32, &local, 1, &remote, 1, 0) } == 16,
        "reading process image identity: {}",
        std::io::Error::last_os_error()
    );
    material[16..].copy_from_slice(&pid.to_ne_bytes());
    let digest = Sha256::digest(material);
    Ok(u64::from_ne_bytes(digest[..8].try_into().unwrap()).max(1))
}

pub(crate) fn open_python_map(pid: u32, instance: u64) -> Result<File> {
    ensure!(
        live_instance(pid)? == instance,
        "Python process image changed before symbol metadata arrived"
    );
    Ok(File::open(format!("/proc/{pid}/root/tmp/perf-{pid}.map"))?)
}

fn python_map(file: &File) -> std::io::Result<PythonSymbols> {
    if file.metadata()?.len() > 8 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Python perf map exceeds 8 MiB",
        ));
    }
    let mut file = file.try_clone()?;
    file.rewind()?;
    let mut entries = Vec::new();
    let mut reader = BufReader::new(file.take(8 * 1024 * 1024));
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        if !line.ends_with('\n') {
            break;
        }
        let mut fields = line.splitn(3, ' ');
        if let (Some(address), Some(size), Some(name)) =
            (fields.next(), fields.next(), fields.next())
        {
            if name.starts_with("py::") {
                if let (Ok(address), Ok(size)) = (
                    u64::from_str_radix(address, 16),
                    u64::from_str_radix(size, 16),
                ) {
                    if let Some(end) = address.checked_add(size) {
                        entries.push((address, end, name.trim_end().chars().take(2048).collect()));
                    }
                }
            }
        }
        line.clear();
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_symbols_wait_for_perf_lifecycle_records_before_binding_an_exec_image() {
        let mut symbols = Symbols::new();
        symbols.images.insert(
            7,
            Image {
                epoch: 10,
                ..Default::default()
            },
        );
        symbols.pin_python(7, 99, 40, Ok(tempfile::tempfile().unwrap()));
        symbols.advance_python(30);
        assert!(!symbols.native_epochs.contains_key(&(7, 99)));
        symbols.images.get_mut(&7).unwrap().epoch = 35;
        symbols.advance_python(40);
        assert_eq!(symbols.native_epochs[&(7, 99)], 35);

        symbols.images.get_mut(&7).unwrap().epoch = 100;
        symbols.advance_python(110);
        symbols.pin_python(7, 101, 50, Ok(tempfile::tempfile().unwrap()));
        assert!(!symbols.native_epochs.contains_key(&(7, 101)));
    }
}
