# ROST: Architecture of a Safety-Critical Microkernel

## A Deep Technical Guide to the Rost Operating System Kernel

---

### Preface

This book is the definitive technical reference for the Rost microkernel — a
formally-reasoned, safety-first operating system kernel for 64-bit x86 hardware,
written entirely in Rust.

Rost began as an experiment in taking the principles of safety-critical embedded
software engineering — IEC 61508 SIL 4, ISO 26262 ASIL D — and applying them
rigorously to a full-featured microkernel that is simultaneously usable as a
daily-driver research OS.  The result is a system in which every design decision
can be traced back to a concrete safety requirement, every module has unit tests,
and the core scheduling and IPC invariants have been formally verified in TLA+.

This is not a "how to write a toy OS" book.  Those books show you how to print
characters on screen and call it a day.  This book traces the complete life of a
system from the moment the UEFI firmware hands control to the kernel binary, through
the transition to protected ring-3 user space, all the way to a running interactive
shell with a TCP/IP network stack — and it explains every single decision along the
way, including the ones that were hard, the tradeoffs that were painful, and the
Rust idioms that made it possible to write safe, `no_std`, bare-metal code without
losing your mind.

**Who this book is for**

- Systems programmers who want to understand how a real OS kernel is structured
- Embedded engineers interested in applying safety standards to system software
- Rust programmers who want to go deep into `no_std`, inline assembly, and bare-metal
- Students studying operating systems who want more than textbook theory

**How to read this book**

Part I (Chapters 1–14) covers the kernel itself — everything that runs in ring 0.
Part II (Chapters 15–22) covers user space — every server, driver, and library that
runs in ring 3.  Chapter 23 covers the build system and toolchain.  Chapter 24
looks at the road ahead.

The chapters are designed to be read in order, but each one is also self-contained
enough that you can jump to any topic if you already have the background context.

---

*"A complex system that works is invariably found to have evolved from a simple system
that worked.  The inverse proposition also appears to be true: a complex system designed
from scratch never works and cannot be made to work."*

— John Gall, Systemantics (1975)

Rost is simple by design.  Read on to find out what "simple" means when you're
trying to meet SIL-4.
