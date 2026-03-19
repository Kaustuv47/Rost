------------------------------ MODULE Memory --------------------------------
(* Physical frame isolation model for the Rost microkernel memory subsystem.
 *
 * This model captures the abstract behaviour of:
 *   crates/core-kernel/src/memory/physical.rs  (PhysicalAllocator)
 *   crates/core-kernel/src/memory/paging.rs    (map_page, identity_map_region)
 *   crates/arch-x86_64/src/cpu/syscall.rs      (SYS_MAP, SYS_MAP_SHARE, SYS_MAP_CAP)
 *
 * Key safety property — FrameIsolation:
 *   No two virtual addresses in different processes map to the same physical
 *   frame unless the kernel explicitly created a shared mapping via
 *   SYS_MAP_SHARE + SYS_MAP_CAP (Memory capability pair).
 *
 * IEC 61508 §7.4.3: spatial isolation between safety partitions.  A memory
 * error in one partition must not corrupt another partition's address space.
 *
 * Tool: TLC model checker  (tlc2 -config Memory.cfg Memory.tla)
 *)

EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxProc,    \* e.g. 4
    MaxFrames   \* e.g. 16 (physical frame count for model checking)

ASSUME MaxProc   \in Nat /\ MaxProc   >= 1
ASSUME MaxFrames \in Nat /\ MaxFrames >= 1

VARIABLES
    mappings,   \* [PID -> [VA -> Frame]]  page tables per process
    shared,     \* SET of Frame — explicitly kernel-shared frames
    free,       \* SET of Frame — available for allocation
    next_frame  \* next frame to allocate (bump allocator index)

Proc  == 1..MaxProc
Frame == 1..MaxFrames \* abstract physical frame numbers
VA    == Nat          \* abstract virtual address (any natural number)

vars == <<mappings, shared, free, next_frame>>

(* ── Type invariant ─────────────────────────────────────────────────────── *)

TypeOK ==
    /\ mappings   \in [Proc -> [VA -> (Frame \cup {0})]]
    /\ shared     \subseteq Frame
    /\ free       \subseteq Frame
    /\ next_frame \in 1..(MaxFrames + 1)

\* Simpler model: mappings is a partial function represented as a set of tuples.
\* We use a flat set for tractability with TLC.
VARIABLES map_set   \* SET of [proc: PID, va: VA, frame: Frame, is_shared: BOOL]

AllVars == <<map_set, shared, free, next_frame>>

MapTypeOK ==
    /\ map_set    \subseteq [proc: Proc, va: VA, frame: Frame, is_shared: BOOLEAN]
    /\ shared     \subseteq Frame
    /\ free       \subseteq Frame
    /\ next_frame \in 1..(MaxFrames + 1)

(* ── Helpers ────────────────────────────────────────────────────────────── *)

\* All frames currently mapped by any process.
MappedFrames == {e.frame : e \in map_set}

\* Frames mapped by a specific process.
FramesOf(p) == {e.frame : e \in {m \in map_set : m.proc = p}}

\* Is frame f mapped by process p at virtual address va?
IsMapped(p, va, f) ==
    \E e \in map_set : e.proc = p /\ e.va = va /\ e.frame = f

\* Allocate the next free frame (bump allocator — no reuse until freed).
NextFreeFrame == next_frame

(* ── Initial state ──────────────────────────────────────────────────────── *)

Init ==
    /\ map_set    = {}
    /\ shared     = {}
    /\ free       = Frame
    /\ next_frame = 1

(* ── Actions ────────────────────────────────────────────────────────────── *)

\* SYS_MAP: kernel maps a private (non-shared) physical frame into process p.
\* Models the PhysicalAllocator bump-allocating a frame and map_page mapping it.
MapPrivate(p, va) ==
    /\ next_frame <= MaxFrames            \* frame available
    /\ ~\E e \in map_set : e.proc = p /\ e.va = va   \* va not already mapped
    /\ LET f == next_frame IN
       /\ map_set'    = map_set \cup {[proc |-> p, va |-> va,
                                       frame |-> f, is_shared |-> FALSE]}
       /\ next_frame' = next_frame + 1
       /\ free'       = free \ {f}
    /\ UNCHANGED shared

\* SYS_MAP_SHARE + SYS_MAP_CAP: kernel allocates a shared frame, maps into
\* owner's address space, and later maps the same frame into another process
\* via a Memory capability.  Both mappings carry is_shared = TRUE.
MapShared(p1, va1, p2, va2) ==
    /\ p1 # p2
    /\ next_frame <= MaxFrames
    /\ ~\E e \in map_set : e.proc = p1 /\ e.va = va1
    /\ ~\E e \in map_set : e.proc = p2 /\ e.va = va2
    /\ LET f == next_frame IN
       /\ map_set'    = map_set
                         \cup {[proc |-> p1, va |-> va1, frame |-> f, is_shared |-> TRUE]}
                         \cup {[proc |-> p2, va |-> va2, frame |-> f, is_shared |-> TRUE]}
       /\ shared'     = shared \cup {f}
       /\ next_frame' = next_frame + 1
       /\ free'       = free \ {f}

\* Unmap a virtual address from a process (e.g. on process exit).
Unmap(p, va) ==
    /\ \E e \in map_set : e.proc = p /\ e.va = va
    /\ LET removed == {e \in map_set : e.proc = p /\ e.va = va}
           f       == (CHOOSE e \in removed : TRUE).frame
           still_mapped == \E e \in (map_set \ removed) : e.frame = f
       IN
       /\ map_set' = map_set \ removed
       /\ free'    = IF still_mapped THEN free ELSE free \cup {f}
       /\ shared'  = IF still_mapped THEN shared ELSE shared \ {f}
    /\ UNCHANGED next_frame

(* ── Specification ──────────────────────────────────────────────────────── *)

Next ==
    \/ \E p \in Proc, va \in 0..7    : MapPrivate(p, va)
    \/ \E p1, p2 \in Proc, va1, va2 \in 0..7 : MapShared(p1, va1, p2, va2)
    \/ \E p \in Proc, va \in 0..7    : Unmap(p, va)

Spec == Init /\ [][Next]_AllVars

(* ── Safety property ────────────────────────────────────────────────────── *)

\* No private frame is mapped by more than one process.
\* Shared frames (is_shared = TRUE) are excluded — they are intentional aliases.
FrameIsolation ==
    \A e1, e2 \in map_set :
        (e1.frame = e2.frame /\ e1.proc # e2.proc)
        => (e1.is_shared = TRUE /\ e2.is_shared = TRUE)

\* A frame that is not in `shared` maps to at most one process.
PrivateFrameExclusive ==
    \A f \in (MappedFrames \ shared) :
        Cardinality({e \in map_set : e.frame = f}) <= 1
        \/
        \* All entries for frame f belong to the same process (private mapping).
        \E p \in Proc : \A e \in map_set : e.frame = f => e.proc = p

THEOREM Spec => []FrameIsolation
THEOREM Spec => []PrivateFrameExclusive

=============================================================================
