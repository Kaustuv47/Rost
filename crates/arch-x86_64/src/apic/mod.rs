/// APIC (Advanced Programmable Interrupt Controller) subsystem.
///
/// Contains:
/// - `lapic`  — Local APIC (xAPIC mode): per-CPU timer, EOI, SVR, LINT masking
/// - `ioapic` — I/O APIC: IRQ routing and redirection table management

pub mod lapic;
pub mod ioapic;
