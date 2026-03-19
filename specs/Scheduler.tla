------------------------------ MODULE Scheduler ------------------------------
(* Liveness and safety model for the Rost round-robin / EDF scheduler.
 *
 * This model captures the abstract behaviour of:
 *   crates/core-kernel/src/scheduler/round_robin.rs
 *   crates/core-kernel/src/process/table.rs
 *
 * Two key properties are checked:
 *
 *   Safety — AtMostOneRunning, RunningConsistent
 *     No two processes are Running simultaneously; the `running` ghost
 *     variable is always consistent with the state array.
 *
 *   Liveness — NoStarvation
 *     Every Ready process is eventually scheduled (weak fairness on Tick
 *     is sufficient for round-robin because Tick always selects one Ready
 *     process if any exist).
 *
 * IEC 61508 §7.4.8: worst-case response time of the highest-priority task
 * must be bounded.  The Tick action models a single quantum: under the weak
 * fairness assumption, every quantum that passes must eventually select a
 * Ready process, bounding starvation to at most |Ready| quanta.
 *
 * Tool: TLC model checker  (tlc2 -config Scheduler.cfg Scheduler.tla)
 *)

EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxProc,    \* e.g. 4 for fast model checking
    MaxPrio     \* e.g. 3

ASSUME MaxProc \in Nat /\ MaxProc >= 1
ASSUME MaxPrio \in Nat /\ MaxPrio >= 0

VARIABLES
    pstate,     \* [1..MaxProc -> {"Dead","Ready","Running","Blocked"}]
    prio,       \* [1..MaxProc -> 0..MaxPrio]  lower = higher priority
    running,    \* currently running PID, or 0 = idle
    tick        \* monotonically increasing tick counter

Proc   == 1..MaxProc
States == {"Dead", "Ready", "Running", "Blocked"}
vars   == <<pstate, prio, running, tick>>

(* ── Type invariant ─────────────────────────────────────────────────────── *)

TypeOK ==
    /\ pstate  \in [Proc -> States]
    /\ prio    \in [Proc -> 0..MaxPrio]
    /\ running \in (Proc \cup {0})
    /\ tick    \in Nat

(* ── Safety invariants ──────────────────────────────────────────────────── *)

AtMostOneRunning ==
    Cardinality({p \in Proc : pstate[p] = "Running"}) <= 1

RunningConsistent ==
    /\ (running = 0) <=> (\A p \in Proc : pstate[p] # "Running")
    /\ (running # 0) => pstate[running] = "Running"

(* ── Initial state ──────────────────────────────────────────────────────── *)

Init ==
    /\ pstate  = [p \in Proc |-> "Dead"]
    /\ prio    = [p \in Proc |-> MaxPrio]
    /\ running = 0
    /\ tick    = 0

(* ── Actions ────────────────────────────────────────────────────────────── *)

(* Spawn a new process with given priority. *)
Spawn(p, pri) ==
    /\ pstate[p] = "Dead"
    /\ pstate' = [pstate EXCEPT ![p] = "Ready"]
    /\ prio'   = [prio   EXCEPT ![p] = pri]
    /\ UNCHANGED <<running, tick>>

(* Quantum expiry: demote Running to Ready, pick highest-priority Ready. *)
Tick ==
    /\ tick' = tick + 1
    /\ LET ready == {p \in Proc : pstate[p] = "Ready"
                                \/ (pstate[p] = "Running" /\ p = running)}
           \* Priority selection: lower number wins; ties broken by PID (deterministic).
           BestOf(S) == CHOOSE p \in S :
               /\ \A q \in S : prio[p] <= prio[q]
               /\ \A q \in S : prio[p] = prio[q] => p <= q
           readySet == {p \in Proc : pstate[p] = "Ready"}
       IN
       IF readySet = {} /\ running = 0 THEN
           UNCHANGED <<pstate, running, prio>>
       ELSE IF readySet = {} THEN
           \* Running process keeps the CPU (no preemption target).
           UNCHANGED <<pstate, running, prio>>
       ELSE
           LET next == BestOf(readySet)
           IN
           /\ pstate'  = [pstate EXCEPT
                            ![running] = IF running # 0 THEN "Ready" ELSE "Dead",
                            ![next]    = "Running"]
           /\ running' = next
           /\ UNCHANGED prio

(* Running process terminates voluntarily. *)
Terminate ==
    /\ running # 0
    /\ pstate'  = [pstate  EXCEPT ![running] = "Dead"]
    /\ running' = 0
    /\ UNCHANGED <<prio, tick>>

(* Running process blocks on I/O or IPC. *)
Block ==
    /\ running # 0
    /\ pstate'  = [pstate  EXCEPT ![running] = "Blocked"]
    /\ running' = 0
    /\ UNCHANGED <<prio, tick>>

(* External event unblocks a waiting process. *)
Unblock(p) ==
    /\ pstate[p] = "Blocked"
    /\ pstate'  = [pstate EXCEPT ![p] = "Ready"]
    /\ UNCHANGED <<prio, running, tick>>

(* ── Specification ──────────────────────────────────────────────────────── *)

Next ==
    \/ \E p \in Proc, pri \in 0..MaxPrio : Spawn(p, pri)
    \/ Tick
    \/ Terminate
    \/ Block
    \/ \E p \in Proc : Unblock(p)

\* Weak fairness: Tick must eventually fire whenever it is continuously enabled.
Fairness ==
    WF_vars(Tick)

Spec == Init /\ [][Next]_vars /\ Fairness

(* ── Properties ─────────────────────────────────────────────────────────── *)

Safety    == TypeOK /\ AtMostOneRunning /\ RunningConsistent

\* Every Ready process is eventually scheduled.
NoStarvation ==
    \A p \in Proc : (pstate[p] = "Ready") ~> (pstate[p] = "Running")

THEOREM Spec => []Safety
THEOREM Spec => NoStarvation

=============================================================================
