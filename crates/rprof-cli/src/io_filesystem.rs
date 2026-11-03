//! Workload-namespace mount labels. The wire identity is authoritative; mountinfo
//! is only a best-effort display label and must never be read from our namespace.
use std::{collections::BTreeMap, fs::File, io::Read, os::unix::fs::MetadataExt};

use rprof_core::stream::Attribute;
use rprof_io_protocol::{
    FS_DEVICE_VALID, FS_MAGIC_VALID, FS_MODE_VALID, FS_MOUNT_ID_VALID, FS_NAMESPACE_VALID, IoEvent,
};

const MAX_MOUNTINFO_BYTES: u64 = 1024 * 1024;
const MAX_MOUNTINFO_LINES: usize = 4096;
const MAX_LABELS: usize = 256;
type MountKey = (u32, u64, u64, u32, u32, u32);
type MountLabel = (String, String);

pub(super) struct Resolver {
    labels: BTreeMap<MountKey, Option<MountLabel>>,
}

impl Resolver {
    pub(super) fn new() -> Self {
        Self {
            labels: BTreeMap::new(),
        }
    }

    fn label(&mut self, event: &IoEvent) -> Option<(String, String)> {
        let valid = FS_NAMESPACE_VALID | FS_MOUNT_ID_VALID | FS_DEVICE_VALID;
        if event.fs_valid & valid != valid {
            return None;
        }
        let key = (
            event.pid,
            event.instance,
            event.mount_namespace,
            event.mount_id,
            event.dev_major,
            event.dev_minor,
        );
        if let Some(label) = self.labels.get(&key) {
            return label.clone();
        }
        let result = Self::read_label(event);
        if self.labels.len() >= MAX_LABELS {
            self.labels.clear();
        }
        // Cache failed lookups too: a short-lived/exited process must not cause
        // a procfs read for every queued I/O observation.
        self.labels.insert(key, result.clone());
        result
    }

    fn read_label(event: &IoEvent) -> Option<(String, String)> {
        // A reused PID is not enough: verify the current mount namespace before
        // consulting its mount table. Once a process exits the label stays unknown.
        let namespace = std::fs::metadata(format!("/proc/{}/ns/mnt", event.pid)).ok()?;
        if namespace.ino() != event.mount_namespace {
            return None;
        }
        let file = File::open(format!("/proc/{}/mountinfo", event.pid)).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_MOUNTINFO_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_MOUNTINFO_BYTES {
            return None;
        }
        // pid can exit or change namespace while we read; recheck after reading.
        if std::fs::metadata(format!("/proc/{}/ns/mnt", event.pid))
            .ok()?
            .ino()
            != event.mount_namespace
        {
            return None;
        }
        mount_label(&bytes, event)
    }

    pub(super) fn attributes(&mut self, event: &IoEvent) -> BTreeMap<String, Attribute> {
        let mode = event.file_mode & libc::S_IFMT;
        let kind = if event.fs_valid & FS_MODE_VALID == 0 {
            "unknown"
        } else if matches!(mode, libc::S_IFREG | libc::S_IFDIR | libc::S_IFLNK) {
            if event.fs_valid & (FS_MAGIC_VALID | FS_DEVICE_VALID)
                == FS_MAGIC_VALID | FS_DEVICE_VALID
            {
                "filesystem"
            } else {
                "unknown"
            }
        } else {
            "non_filesystem"
        };
        let device = if event.fs_valid & FS_DEVICE_VALID != 0 {
            format!("{}:{}", event.dev_major, event.dev_minor)
        } else {
            "unknown".into()
        };
        let namespace = if event.fs_valid & FS_NAMESPACE_VALID != 0 {
            event.mount_namespace.to_string()
        } else {
            "unknown".into()
        };
        let label = if kind == "filesystem" {
            self.label(event)
        } else {
            None
        };
        let fs_type = if kind != "filesystem" {
            "unknown".to_owned()
        } else {
            label
                .as_ref()
                .map(|(_, kind)| kind.clone())
                .unwrap_or_else(|| {
                    filesystem_name(event.fs_magic)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("0x{:x}", event.fs_magic))
                })
        };
        let id = if kind == "filesystem" {
            let ns = if event.fs_valid & FS_NAMESPACE_VALID != 0 {
                format!("ns:{}", event.mount_namespace)
            } else {
                // Partial identity: don't split a genuinely shared mount per PID.
                // Its unknown namespace remains visible to consumers.
                "ns:unknown".to_owned()
            };
            let mount = if event.fs_valid & FS_MOUNT_ID_VALID != 0 {
                format!("mnt:{}", event.mount_id)
            } else {
                "mnt:unknown".to_owned()
            };
            format!("{ns}:{mount}:dev:{device}:magic:{:016x}", event.fs_magic)
        } else {
            "unknown".to_owned()
        };
        BTreeMap::from([
            ("io.filesystem.type".into(), Attribute::Text(fs_type)),
            (
                "io.filesystem.mount_point".into(),
                Attribute::Text(label.map_or_else(|| "unknown".into(), |(point, _)| point)),
            ),
            ("io.filesystem.device".into(), Attribute::Text(device)),
            ("io.filesystem.id".into(), Attribute::Text(id)),
            ("io.filesystem.kind".into(), Attribute::Text(kind.into())),
            ("io.filesystem.namespace".into(), Attribute::Text(namespace)),
            (
                "io.filesystem.attribution_partial".into(),
                Attribute::Bool(
                    kind == "filesystem"
                        && event.fs_valid & (FS_MOUNT_ID_VALID | FS_NAMESPACE_VALID)
                            != (FS_MOUNT_ID_VALID | FS_NAMESPACE_VALID),
                ),
            ),
        ])
    }
}

fn mount_label(bytes: &[u8], event: &IoEvent) -> Option<(String, String)> {
    for line in bytes.split(|byte| *byte == b'\n').take(MAX_MOUNTINFO_LINES) {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let mut fields = before.split_whitespace();
        let (Some(id), Some(_parent), Some(device), Some(_root), Some(point)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let mut dev = device.split(':');
        if id.parse::<u32>().ok() != Some(event.mount_id)
            || dev.next().and_then(|v| v.parse::<u32>().ok()) != Some(event.dev_major)
            || dev.next().and_then(|v| v.parse::<u32>().ok()) != Some(event.dev_minor)
            || dev.next().is_some()
        {
            continue;
        }
        let fs_type = after.split_whitespace().next().unwrap_or("unknown");
        if point.len() > 4096 || fs_type.len() > 128 {
            return None;
        }
        return Some((unescape(point), fs_type.to_owned()));
    }
    None
}

fn filesystem_name(magic: u64) -> Option<&'static str> {
    Some(match magic as u32 {
        0xef53 => "ext2/3/4",
        0x58465342 => "xfs",
        0x01021994 => "tmpfs",
        0x794c7630 => "overlay",
        0x9123683e => "btrfs",
        0x6969 => "nfs",
        0x65735546 => "fuse",
        _ => return None,
    })
}

fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
            && bytes[i + 1] <= b'3'
        {
            decoded
                .push((bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + bytes[i + 3] - b'0');
            i += 4;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mountinfo_uses_device_after_parent_id_and_decodes_escaped_mounts() {
        let mut event: IoEvent = unsafe { std::mem::zeroed() };
        event.mount_id = 42;
        event.dev_major = 8;
        event.dev_minor = 17;
        let input = b"42 29 8:17 /subroot /data\\040files rw shared:1 - ext4 /dev/sdb1 rw\n";
        assert_eq!(
            mount_label(input, &event),
            Some(("/data files".into(), "ext4".into()))
        );
        event.dev_minor = 18;
        assert!(mount_label(input, &event).is_none());
        assert_eq!(unescape(r"a\777b"), r"a\777b");
    }

    #[test]
    fn live_workload_namespace_resolves_a_real_mount() {
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().unwrap();
        let mut sx: libc::statx = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::statx(
                file.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                libc::STATX_MNT_ID,
                &mut sx,
            )
        } != 0
            || sx.stx_mask & libc::STATX_MNT_ID == 0
        {
            return;
        }
        let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstatfs(file.as_raw_fd(), &mut fs) }, 0);
        let mut event: IoEvent = unsafe { std::mem::zeroed() };
        event.pid = std::process::id();
        event.fs_valid = FS_MODE_VALID
            | FS_DEVICE_VALID
            | FS_MAGIC_VALID
            | FS_NAMESPACE_VALID
            | FS_MOUNT_ID_VALID;
        event.file_mode = libc::S_IFREG;
        event.dev_major = sx.stx_dev_major;
        event.dev_minor = sx.stx_dev_minor;
        event.mount_id = sx.stx_mnt_id as u32;
        event.mount_namespace = std::fs::metadata("/proc/self/ns/mnt").unwrap().ino();
        event.fs_magic = fs.f_type as u64;
        let mut resolver = Resolver::new();
        let attrs = resolver.attributes(&event);
        assert!(
            matches!(&attrs["io.filesystem.mount_point"], Attribute::Text(value) if value.starts_with('/'))
        );
        assert!(
            matches!(&attrs["io.filesystem.type"], Attribute::Text(value) if value != "unknown")
        );
        assert_eq!(resolver.labels.len(), 1);
    }
}
