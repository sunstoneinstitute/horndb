//! Host CPU detection and the known-CPU kernel table.
//!
//! Real SPB-256 measurements proved the micro-calibrated SIMD kernels are net
//! *harmful* versus scalar on the CPUs we run on (both AMD Zen4 and Intel
//! Sapphire Rapids): the balanced, L2-resident calibration inputs are
//! unrepresentative of the skewed, memory-bound shapes production dispatches. So
//! before falling back to calibration, each primitive consults an authoritative
//! per-CPU table keyed on the host's CPUID vendor/family/model. A table hit
//! selects a kernel with **no timing**; a miss falls through to calibration.
//!
//! The table is intentionally per-`(cpu, kernel)`: today both known rows pin
//! *every* kernel to scalar, but the shape supports future per-kernel SIMD
//! entries without a signature change.
//!
//! ## Host identity across arches
//!
//! [`detect`] returns a [`CpuKey`] on the two arches we can identify cheaply:
//! **x86_64** via CPUID (Intel + AMD, any OS — so a Linux EPYC box gets a real
//! identity even without a table row), and **aarch64 macOS** (Apple Silicon)
//! via `sysctlbyname` (`hw.cpufamily`/`hw.cpusubfamily`). Everywhere else
//! (e.g. aarch64 Linux) there is no cheap identity and it returns `None`,
//! falling through to calibration. [`identity`] additionally exposes the
//! human-readable brand string (CPUID leaves `0x8000_0002..4` on x86, the
//! `machdep.cpu.brand_string` sysctl on Apple) for startup logging. A table hit
//! still requires a per-`(cpu, kernel)` row; today Apple has none, so Apple
//! hosts get an identity but still calibrate.
//!
//! This crate stays dependency-free: the macOS path calls `sysctlbyname`
//! through a raw `extern "C"` declaration (it lives in libSystem, linked by
//! default), not via a `libc`/`sysctl` crate.

use crate::dispatch::Isa;
use std::sync::OnceLock;

/// CPU vendor. `Intel`/`Amd`/`Other` are decoded from the x86 CPUID leaf-0
/// vendor string; `Apple` is assigned on aarch64 macOS (Apple Silicon), which
/// is identified via sysctl rather than CPUID.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Vendor {
    Intel,
    Amd,
    Apple,
    Other,
}

impl Vendor {
    /// Stable, human-readable vendor name for the identity-fallback string.
    #[cfg_attr(
        not(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", target_os = "macos")
        )),
        allow(dead_code)
    )]
    fn name(self) -> &'static str {
        match self {
            Vendor::Intel => "Intel",
            Vendor::Amd => "AMD",
            Vendor::Apple => "Apple",
            Vendor::Other => "unknown-vendor",
        }
    }
}

/// A host CPU identity: vendor plus the CPUID display family/model (the
/// extended-arithmetic values, not the raw base nibbles).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CpuKey {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
}

/// The primitives this crate dispatches. `name` returns the stable
/// [`crate::calibration_report`] string so downstream logging/label use is
/// unaffected by this enum's introduction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kernel {
    Intersect,
    LowerBound,
    Merge,
    Dedup,
    FilterRange,
    FilterIndicesEq,
    Gather,
}

impl Kernel {
    /// The stable, human-readable name of this kernel, matching the strings in
    /// [`crate::calibration_report`].
    pub fn name(self) -> &'static str {
        match self {
            Kernel::Intersect => "intersect",
            Kernel::LowerBound => "lower_bound",
            Kernel::Merge => "merge",
            Kernel::Dedup => "dedup",
            Kernel::FilterRange => "filter_range",
            Kernel::FilterIndicesEq => "filter_indices_eq",
            Kernel::Gather => "gather",
        }
    }
}

/// Decode a CPUID leaf-1 `EAX` word into `(display_family, display_model)` using
/// the standard extended arithmetic. Pure and testable without CPUID.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn decode_family_model(eax: u32) -> (u32, u32) {
    let base_family = (eax >> 8) & 0xF;
    let base_model = (eax >> 4) & 0xF;
    let display_family = if base_family == 0xF {
        base_family + ((eax >> 20) & 0xFF)
    } else {
        base_family
    };
    let display_model = if base_family == 0x6 || base_family == 0xF {
        base_model + (((eax >> 16) & 0xF) << 4)
    } else {
        base_model
    };
    (display_family, display_model)
}

/// Decode the CPUID leaf-0 vendor registers `(EBX, EDX, ECX)` into a [`Vendor`].
/// The 12-byte vendor string is `EBX ++ EDX ++ ECX` in little-endian byte order.
/// Pure and testable without CPUID.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn decode_vendor(ebx: u32, edx: u32, ecx: u32) -> Vendor {
    let mut s = [0u8; 12];
    s[0..4].copy_from_slice(&ebx.to_le_bytes());
    s[4..8].copy_from_slice(&edx.to_le_bytes());
    s[8..12].copy_from_slice(&ecx.to_le_bytes());
    match &s {
        b"GenuineIntel" => Vendor::Intel,
        b"AuthenticAMD" => Vendor::Amd,
        _ => Vendor::Other,
    }
}

/// Read the host CPU identity via CPUID (x86_64) or report `None` where CPUID is
/// inaccessible (non-x86_64). Memoised.
pub(crate) fn detect() -> Option<CpuKey> {
    static CACHE: OnceLock<Option<CpuKey>> = OnceLock::new();
    *CACHE.get_or_init(detect_uncached)
}

#[cfg(target_arch = "x86_64")]
fn detect_uncached() -> Option<CpuKey> {
    // Safety: `__cpuid` is always available on x86_64 (CPUID has been baseline
    // since the mid-90s); it reads no memory and has no preconditions.
    let (vendor, eax) = unsafe {
        use std::arch::x86_64::__cpuid;
        let leaf0 = __cpuid(0);
        let vendor = decode_vendor(leaf0.ebx, leaf0.edx, leaf0.ecx);
        let leaf1 = __cpuid(1);
        (vendor, leaf1.eax)
    };
    let (family, model) = decode_family_model(eax);
    Some(CpuKey {
        vendor,
        family,
        model,
    })
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn detect_uncached() -> Option<CpuKey> {
    // Apple Silicon has no userspace CPUID; its stable identity is the
    // `hw.cpufamily` hash (per chip generation) plus `hw.cpusubfamily`. Map
    // them onto the same `(vendor, family, model)` shape the table keys on, so a
    // future measured Apple row slots in exactly like the x86 rows.
    let family = sysctl_u32("hw.cpufamily")?;
    let model = sysctl_u32("hw.cpusubfamily").unwrap_or(0);
    Some(CpuKey {
        vendor: Vendor::Apple,
        family,
        model,
    })
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_os = "macos")
)))]
fn detect_uncached() -> Option<CpuKey> {
    // No cheap identity here (e.g. aarch64 Linux): fall through to calibration.
    None
}

/// The host's human-readable CPU identity — the brand string where available
/// (CPUID leaves `0x8000_0002..4` on x86, `machdep.cpu.brand_string` on Apple),
/// else a `"<vendor> family <f> model <m>"` fallback synthesised from
/// [`detect`]. `None` only when the host is unidentifiable on this arch (e.g.
/// aarch64 Linux). Surfaced by [`crate::cpu_identity`] for startup logging so a
/// host that fell through to calibration still reports *which* CPU it is.
pub(crate) fn identity() -> Option<String> {
    if let Some(brand) = brand_string() {
        let brand = brand.trim();
        if !brand.is_empty() {
            return Some(brand.to_string());
        }
    }
    detect().map(|c| format!("{} family {} model {}", c.vendor.name(), c.family, c.model))
}

/// Trim an x86 CPUID brand string: cut at the first NUL, then strip the
/// space-padding CPUID applies. Pure and testable without CPUID.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn parse_brand_bytes(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// The raw CPU brand string, or `None` on an arch without one.
#[cfg(target_arch = "x86_64")]
fn brand_string() -> Option<String> {
    use std::arch::x86_64::__cpuid;
    // Safety: CPUID is baseline on x86_64; these reads have no preconditions.
    let max_ext = unsafe { __cpuid(0x8000_0000) }.eax;
    if max_ext < 0x8000_0004 {
        return None; // brand-string leaves unsupported (vanishingly rare)
    }
    let mut bytes = [0u8; 48];
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004]
        .iter()
        .enumerate()
    {
        let r = unsafe { __cpuid(*leaf) };
        let off = i * 16;
        bytes[off..off + 4].copy_from_slice(&r.eax.to_le_bytes());
        bytes[off + 4..off + 8].copy_from_slice(&r.ebx.to_le_bytes());
        bytes[off + 8..off + 12].copy_from_slice(&r.ecx.to_le_bytes());
        bytes[off + 12..off + 16].copy_from_slice(&r.edx.to_le_bytes());
    }
    Some(parse_brand_bytes(&bytes))
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn brand_string() -> Option<String> {
    sysctl_string("machdep.cpu.brand_string")
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_os = "macos")
)))]
fn brand_string() -> Option<String> {
    None
}

// --- macOS sysctl (raw FFI, no `libc` dependency) --------------------------

// `sysctlbyname` from libSystem (linked by default on macOS). Reads a named
// MIB into a caller buffer; a null `oldp` with a `&mut len` asks for the size.
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
extern "C" {
    fn sysctlbyname(
        name: *const std::os::raw::c_char,
        oldp: *mut std::os::raw::c_void,
        oldlenp: *mut usize,
        newp: *mut std::os::raw::c_void,
        newlen: usize,
    ) -> std::os::raw::c_int;
}

/// Read a string-typed sysctl by name (e.g. `machdep.cpu.brand_string`).
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn sysctl_string(name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    // First call with a null buffer reports the byte length (incl. trailing NUL).
    let mut len: usize = 0;
    let rc = unsafe {
        sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let rc = unsafe {
        sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr() as *mut std::os::raw::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    if buf.last() == Some(&0) {
        buf.pop(); // drop the C trailing NUL
    }
    String::from_utf8(buf).ok()
}

/// Read a 32-bit integer sysctl by name (e.g. `hw.cpufamily`).
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn sysctl_u32(name: &str) -> Option<u32> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut val: u32 = 0;
    let mut len: usize = std::mem::size_of::<u32>();
    let rc = unsafe {
        sysctlbyname(
            cname.as_ptr(),
            &mut val as *mut u32 as *mut std::os::raw::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<u32>() {
        return None;
    }
    Some(val)
}

/// The authoritative kernel choice for a known host CPU, or `None` for an
/// unknown CPU (or a known CPU with no entry for this kernel). Structured as
/// `(vendor, family, model)` → per-[`Kernel`] choice so future per-kernel SIMD
/// entries slot in without a signature change.
pub(crate) fn table_pick(cpu: CpuKey, k: Kernel) -> Option<Isa> {
    match (cpu.vendor, cpu.family, cpu.model) {
        // AMD Zen4 (Ryzen 7 7700). SPB-256: scalar 36.0 vs calibrated 28.6 qps
        // — every calibrated SIMD kernel loses, so pin all to scalar.
        (Vendor::Amd, 25, 97) => Some(scalar_all(k)),
        // Intel Sapphire Rapids (Xeon Gold 5412U). SPB-256: scalar 34.4 vs
        // all-AVX2 17.3 qps — every SIMD kernel loses, so pin all to scalar.
        (Vendor::Intel, 6, 143) => Some(scalar_all(k)),
        _ => None,
    }
}

/// Helper for known CPUs where every kernel is pinned to scalar. Keeps the
/// per-kernel dimension explicit at the call site.
fn scalar_all(_k: Kernel) -> Isa {
    Isa::Scalar
}

/// Detect the host and look up its authoritative kernel choice for `k`, or
/// `None` when the host is unknown / has no accessible CPUID. Each primitive's
/// `choose()` calls this before calibrating: on a hit it picks the matching
/// candidate with no timing.
pub(crate) fn table_isa(k: Kernel) -> Option<Isa> {
    detect().and_then(|cpu| table_pick(cpu, k))
}

/// Pick the `(Isa, kernel)` pair for the host's authoritative table choice for
/// `k`, but only if that ISA survived the caller's capped candidate build.
/// Returns `None` on a table miss or when the table ISA isn't a candidate, so
/// the caller falls through to calibration. Shared by all primitives' `choose()`.
pub(crate) fn table_pick_pair<F: Copy>(cands: &[(Isa, F)], k: Kernel) -> Option<(Isa, F)> {
    let isa = table_isa(k)?;
    cands.iter().copied().find(|(i, _)| *i == isa)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_does_not_panic() {
        // Values vary by host/CI runner; only assert it doesn't panic and is
        // stable across calls (memoised).
        let a = detect();
        let b = detect();
        assert_eq!(a, b);
    }

    #[test]
    fn decode_intel_sapphire_rapids_family6_model143() {
        // Sapphire Rapids: display family 6, display model 143 (0x8F).
        // base_family = 6, base_model = 0xF, ext_model = 0x8 → model = 0x8F = 143.
        // EAX layout: ext_model in bits 19:16, base_family in 11:8, base_model 7:4.
        let eax = (0x8 << 16) | (0x6 << 8) | (0xF << 4);
        assert_eq!(decode_family_model(eax), (6, 143));
    }

    #[test]
    fn decode_amd_zen4_family25_model97() {
        // Zen4: display family 25 (0x19), display model 97 (0x61).
        // base_family = 0xF, ext_family = 25 - 15 = 10 (0xA).
        // base_model = 0x1, ext_model = 0x6 → model = 0x61 = 97.
        let eax = (0x6 << 16) | (0xA << 20) | (0xF << 8) | (0x1 << 4);
        assert_eq!(decode_family_model(eax), (25, 97));
    }

    #[test]
    fn decode_family_model_no_extension_when_base_below_6() {
        // base_family = 5 (< 6 and != 0xF): no extended model applied even if the
        // ext_model bits are set; no extended family since base != 0xF.
        let eax = (0x7 << 16) | (0x5 << 8) | (0x3 << 4);
        assert_eq!(decode_family_model(eax), (5, 3));
    }

    #[test]
    fn decode_vendor_strings() {
        // "GenuineIntel" = EBX "Genu", EDX "ineI", ECX "ntel".
        let ebx = u32::from_le_bytes(*b"Genu");
        let edx = u32::from_le_bytes(*b"ineI");
        let ecx = u32::from_le_bytes(*b"ntel");
        assert_eq!(decode_vendor(ebx, edx, ecx), Vendor::Intel);

        // "AuthenticAMD" = EBX "Auth", EDX "enti", ECX "cAMD".
        let ebx = u32::from_le_bytes(*b"Auth");
        let edx = u32::from_le_bytes(*b"enti");
        let ecx = u32::from_le_bytes(*b"cAMD");
        assert_eq!(decode_vendor(ebx, edx, ecx), Vendor::Amd);

        assert_eq!(decode_vendor(0, 0, 0), Vendor::Other);
    }

    #[test]
    fn table_pins_known_cpus_to_scalar_all_kernels() {
        let kernels = [
            Kernel::Intersect,
            Kernel::LowerBound,
            Kernel::Merge,
            Kernel::Dedup,
            Kernel::FilterRange,
            Kernel::FilterIndicesEq,
            Kernel::Gather,
        ];
        let zen4 = CpuKey {
            vendor: Vendor::Amd,
            family: 25,
            model: 97,
        };
        let spr = CpuKey {
            vendor: Vendor::Intel,
            family: 6,
            model: 143,
        };
        for k in kernels {
            assert_eq!(table_pick(zen4, k), Some(Isa::Scalar), "{k:?}");
            assert_eq!(table_pick(spr, k), Some(Isa::Scalar), "{k:?}");
        }
    }

    #[test]
    fn table_misses_unknown_cpu() {
        let unknown = CpuKey {
            vendor: Vendor::Other,
            family: 6,
            model: 143,
        };
        assert_eq!(table_pick(unknown, Kernel::Intersect), None);
        // Known vendor/family but unknown model.
        let other_intel = CpuKey {
            vendor: Vendor::Intel,
            family: 6,
            model: 42,
        };
        assert_eq!(table_pick(other_intel, Kernel::Intersect), None);
    }

    #[test]
    fn parse_brand_bytes_trims_nul_and_padding() {
        // CPUID space-pads and NUL-terminates; both must be stripped.
        let mut b = *b"   AMD Ryzen 7 7700 8-Core Processor\0\0\0\0\0\0\0\0\0\0\0\0";
        assert_eq!(parse_brand_bytes(&b), "AMD Ryzen 7 7700 8-Core Processor");
        // No NUL at all: still trims the padding.
        b[36] = b' ';
        let full = b"Intel(R) Xeon(R) Gold 5412U             ";
        assert_eq!(parse_brand_bytes(full), "Intel(R) Xeon(R) Gold 5412U");
        // Empty / all-NUL yields empty (identity() then falls back to the key).
        assert_eq!(parse_brand_bytes(&[0u8; 48]), "");
    }

    #[test]
    fn identity_is_stable_and_nonempty_where_detectable() {
        // On x86_64 and Apple Silicon the host is identifiable, so identity() is
        // Some and non-empty; elsewhere (e.g. aarch64 Linux) None is acceptable.
        let id = identity();
        if let Some(s) = &id {
            assert!(!s.is_empty(), "identity must not be an empty string");
        }
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", target_os = "macos")
        ))]
        assert!(id.is_some(), "x86_64 / Apple hosts must be identifiable");
    }

    #[test]
    fn vendor_names_are_stable() {
        assert_eq!(Vendor::Intel.name(), "Intel");
        assert_eq!(Vendor::Amd.name(), "AMD");
        assert_eq!(Vendor::Apple.name(), "Apple");
        assert_eq!(Vendor::Other.name(), "unknown-vendor");
    }

    #[test]
    fn kernel_names_are_stable() {
        assert_eq!(Kernel::Intersect.name(), "intersect");
        assert_eq!(Kernel::LowerBound.name(), "lower_bound");
        assert_eq!(Kernel::Merge.name(), "merge");
        assert_eq!(Kernel::Dedup.name(), "dedup");
        assert_eq!(Kernel::FilterRange.name(), "filter_range");
        assert_eq!(Kernel::FilterIndicesEq.name(), "filter_indices_eq");
        assert_eq!(Kernel::Gather.name(), "gather");
    }
}
