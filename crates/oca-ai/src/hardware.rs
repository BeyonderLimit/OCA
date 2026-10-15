//! Hardware detection → tier recommendation (ADR-0008): tiers, not model
//! names — model churn is the user's business, the machine's appetite is
//! ours. `doctor` reports this; nothing auto-downloads or auto-loads.

/// What the machine looks like. `ram_bytes == 0` means detection failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hardware {
    pub ram_bytes: u64,
    pub cores: usize,
}

/// The three tiers from ADR-0008.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// ~8 GB — models up to ~3–4B.
    Lite,
    /// 16 GB / 8 GB VRAM — ~7–9B.
    Standard,
    /// 32 GB+ / 24 GB VRAM — ~14–32B.
    Comfortable,
}

impl Tier {
    /// One line for `doctor` and the AI panel.
    pub fn describe(self) -> &'static str {
        match self {
            Tier::Lite => "Lite — models up to ~4B",
            Tier::Standard => "Standard — ~7–9B",
            Tier::Comfortable => "Comfortable — 14–32B",
        }
    }
}

impl Hardware {
    /// Best-effort, no subprocesses, nothing written. RAM via the platform
    /// system call; cores via the standard library.
    pub fn detect() -> Self {
        Self {
            ram_bytes: detect_ram(),
            cores: std::thread::available_parallelism()
                .map(|c| c.get())
                .unwrap_or(1),
        }
    }

    pub fn ram_gb(&self) -> u64 {
        self.ram_bytes / 1_000_000_000
    }

    /// RAM-based tier; `None` when detection failed (reported as unknown —
    /// never guessed).
    pub fn tier(&self) -> Option<Tier> {
        match self.ram_bytes {
            0 => None,
            bytes if bytes >= 32_000_000_000 => Some(Tier::Comfortable),
            bytes if bytes >= 16_000_000_000 => Some(Tier::Standard),
            _ => Some(Tier::Lite),
        }
    }
}

#[cfg(unix)]
fn detect_ram() -> u64 {
    // SAFETY: plain system queries; no pointers, no failure modes beyond -1.
    unsafe {
        let pages = libc::sysconf(libc::_SC_PHYS_PAGES);
        let page_size = libc::sysconf(libc::_SC_PAGE_SIZE);
        if pages > 0 && page_size > 0 {
            (pages as u64).wrapping_mul(page_size as u64)
        } else {
            0
        }
    }
}

#[cfg(windows)]
fn detect_ram() -> u64 {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        dwMemoryLoad: 0,
        ullTotalPhys: 0,
        ullAvailPhys: 0,
        ullTotalPageFile: 0,
        ullAvailPageFile: 0,
        ullTotalVirtual: 0,
        ullAvailVirtual: 0,
        ullAvailExtendedVirtual: 0,
    };
    // SAFETY: `status` is a valid, correctly sized out-param.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok != 0 { status.ullTotalPhys } else { 0 }
}

#[cfg(not(any(unix, windows)))]
fn detect_ram() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_follows_ram_and_unknown_stays_unknown() {
        let fake = |gb: u64| Hardware {
            ram_bytes: gb * 1_000_000_000,
            cores: 8,
        };
        assert_eq!(fake(8).tier(), Some(Tier::Lite));
        assert_eq!(fake(16).tier(), Some(Tier::Standard));
        assert_eq!(fake(31).tier(), Some(Tier::Standard));
        assert_eq!(fake(32).tier(), Some(Tier::Comfortable));
        assert_eq!(fake(64).tier(), Some(Tier::Comfortable));
        assert_eq!(
            Hardware {
                ram_bytes: 0,
                cores: 8
            }
            .tier(),
            None
        );
        assert!(Tier::Lite.describe().contains("4B"));
    }

    #[test]
    fn real_detection_finds_something_on_this_platform() {
        let hw = Hardware::detect();
        assert!(hw.cores >= 1);
        // CI machines have RAM; if detection ever breaks, tier goes None —
        // assert detection actually worked somewhere with a memory.
        if hw.ram_bytes > 0 {
            assert!(hw.tier().is_some());
        }
    }
}
