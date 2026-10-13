//! Painting the screen write-combining on AMD64.
//!
//! Firmware usually leaves the framebuffer uncached (see `memtype`), where
//! every store is a bus transaction of its own: a real PC's screen then
//! shows it being painted from the top down. So the loader paints through
//! page tables of its own, in which the framebuffer is write-combining by
//! its page attributes (the PAT), which the MTRRs cannot overrule: the
//! stores go out in bursts, and the screen is black at once. Interrupts
//! are off meanwhile, and the firmware's page tables and PAT come back
//! after.

use super::paging::{PageTables, WRITABLE};
use super::{memtype, read_msr, supports_1g_pages, ticks, write_msr};
use crate::FrameSource;
use crate::blank::{Surface, clear};

/// Where the loader's own page tables map the framebuffer: PML4 slot 384,
/// far above the identity map.
const FRAMEBUFFER_VIRT: u64 = 0xFFFF_C000_0000_0000;
/// A page's `PWT` bit alone: PAT entry 1, write-combining while painting.
const PWT: u64 = 1 << 3;
/// The PAT's encoding of write-combining.
const PAT_WC: u64 = 1;

/// Clears the screen write-combining, through page tables of the loader's
/// own: an identity map of `[0, limit)` (all memory, as the firmware's),
/// and the framebuffer at [`FRAMEBUFFER_VIRT`] through PAT entry 1, made
/// write-combining for the while. Returns the clock counter's ticks the
/// painting took, once the tables were made.
pub fn clear_write_combining<F: FrameSource>(frames: &mut F, s: &Surface, limit: u64) -> Result<u64, &'static str> {
    if !memtype::has_pat() {
        return Err("the processor has no page attribute table");
    }
    if memtype::five_levels() {
        return Err("the firmware pages with five levels");
    }
    let mut pt = PageTables::new(frames, supports_1g_pages())?;
    pt.map_linear(0, limit, WRITABLE)?;
    let base = s.base as u64;
    let bytes = (s.stride as u64 * s.height as u64 * 4 + (base & 0xFFF)).next_multiple_of(4096);
    for page in (0..bytes).step_by(4096) {
        pt.map_4k(FRAMEBUFFER_VIRT + page, (base & !0xFFF) + page, WRITABLE | PWT)?;
    }
    let wc = Surface { base: (FRAMEBUFFER_VIRT + (base & 0xFFF)) as *mut u32, ..*s };
    let tables = pt.pml4;
    let start = ticks();
    // SAFETY: the loader's tables map everything the painting touches as
    // the firmware's do (code, stack, the descriptor tables: all memory
    // below `limit`), and the framebuffer at its alias. Interrupts
    // are off while they are in use; the caches are flushed around the
    // change of the PAT, and every translation (global ones too, by
    // toggling CR4.PGE) around each switch, as the processor's manual asks
    // (Intel SDM, volume 3, 12.12.4).
    unsafe {
        let (flags, cr3, cr4): (u64, u64, u64);
        core::arch::asm!("pushfq", "pop {}", "cli", out(reg) flags);
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nostack));
        let pat = read_msr(memtype::IA32_PAT);
        core::arch::asm!("wbinvd", options(nostack));
        write_msr(memtype::IA32_PAT, (pat & !(0xFF << 8)) | (PAT_WC << 8));
        core::arch::asm!("mov cr4, {}", in(reg) cr4 & !(1 << 7), options(nostack));
        core::arch::asm!("mov cr3, {}", in(reg) tables, options(nostack));
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        clear(&wc);
        // What sits in the write-combining buffers goes out.
        core::arch::asm!("sfence", options(nostack));
        core::arch::asm!("mov cr4, {}", in(reg) cr4 & !(1 << 7), options(nostack));
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack));
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        core::arch::asm!("wbinvd", options(nostack));
        write_msr(memtype::IA32_PAT, pat);
        if flags & (1 << 9) != 0 {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }
    Ok(ticks() - start)
}
