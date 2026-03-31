/*
 * Linker script for Rost ring-3 C binaries.
 *
 * Produces a static ET_EXEC ELF64 loaded at 0x400000 (conventional x86_64
 * base for non-PIE executables).  The Rost ELF loader maps every PT_LOAD
 * segment into the user address space and jumps to e_entry.
 *
 * Constraints:
 *   - No dynamic sections (no .plt, .got.plt, .interp) — Rost has no dynamic
 *     linker.
 *   - No relocations — relocation-model=static / -fno-pie means all
 *     addresses are absolute at link time.
 *   - .bss is zeroed by the ELF loader (p_filesz < p_memsz on the PT_LOAD).
 */

OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)

ENTRY(_start)

SECTIONS
{
    . = 0x400000;

    .text : {
        *(.text.startup)    /* keep _start at the very front */
        *(.text*)
    }

    .rodata : {
        *(.rodata*)
    }

    . = ALIGN(0x1000);

    .data : {
        *(.data*)
    }

    .bss : {
        *(.bss*)
        *(COMMON)
    }

    /* Discard everything the Rost ELF loader doesn't need. */
    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
