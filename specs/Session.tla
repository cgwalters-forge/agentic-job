----------------------------- MODULE Session -----------------------------
EXTENDS Naturals, Sequences
\* Writes during a session (session.md): apply runs while the session runs,
\* once per request the operator confirms, and again at the run's end.
\* Constants select negative controls, NOT alternative production designs.
CONSTANTS TrustInputs, NoPrefix, NoGuard, RenameAtEnd, RunUntrusted
VARIABLES file, phase, accepted, history, credential, executes
vars == <<file, phase, accepted, history, credential, executes>>
Max == 2    \* the run's output ceiling
Lines == 3  \* the most requests the agent writes here, one over the ceiling
Kinds == {"comment", "invalid"}

Init == /\ file = <<>> /\ phase = "session" /\ accepted = {}
        /\ history = <<>> /\ credential = FALSE /\ executes = FALSE

\* run reads the agent's outputs append-only. When the operator confirms
\* request n, it uploads what it has read up to and including n: SubSeq(file,
\* 1, n). The model lets any request be dispatched, confirmed or not.
\* A line once read is never changed, so neither is an upload.
Write == /\ phase = "session" /\ Len(file) < Lines
         /\ \E k \in Kinds: file' = Append(file, k)
         /\ UNCHANGED <<phase, accepted, history, credential, executes>>

\* check's code refuses the whole upload for one invalid request, or for
\* more requests than the ceiling.
Passes(s) == /\ \A i \in 1..Len(s): s[i] # "invalid"
             /\ Len(s) <= Max
\* apply's guard: the name RUNID/POSITION/DIGEST hidden in what it posted.
\* The record is append-only, so a position names one request. The digest is
\* of the request as check accepted it; the broken variant takes it after the
\* end-of-run apply put a partial run's note in the body.
Name(p, end) == <<p, IF RenameAtEnd /\ end THEN "noted" ELSE "checked">>
Applied(nm) == \E n \in 1..Len(history): history[n].name = nm
Effect(p, k, end) == [pos |-> p, kind |-> k, name |-> Name(p, end)]
\* Each apply job holds the write credential, and treats requests as data.
Writer == credential' = TRUE /\ executes' = RunUntrusted

\* The pane dispatches tracker's in-session apply for request n, at any time,
\* even after the session ended. The job fetches n's upload from the run and
\* checks all of it; the broken variants take the request the dispatch
\* inputs carry, or check request n alone.
InSession ==
  /\ \E n \in 1..Len(file):
     LET upload == IF NoPrefix THEN <<file[n]>> ELSE SubSeq(file, 1, n) IN
     IF TrustInputs
     THEN \E k \in Kinds: /\ ~Applied(Name(n, FALSE))
                          /\ history' = Append(history, Effect(n, k, FALSE))
                          /\ UNCHANGED accepted
     ELSE /\ Passes(upload)
          /\ accepted' = accepted \cup {n}
          /\ IF Applied(Name(n, FALSE)) THEN UNCHANGED history
             ELSE history' = Append(history, Effect(n, file[n], FALSE))
  /\ Writer /\ UNCHANGED <<file, phase>>

Stop == /\ phase = "session" /\ phase' = "check"
        /\ UNCHANGED <<file, accepted, history, credential, executes>>

\* The end-of-run check, on another machine, of everything run read.
EndCheck == /\ phase = "check"
            /\ IF Passes(file)
               THEN phase' = "apply" /\ accepted' = accepted \cup DOMAIN file
               ELSE phase' = "done" /\ UNCHANGED accepted
            /\ UNCHANGED <<file, history, credential, executes>>

\* The end-of-run apply leaves out what any apply posted before; the broken
\* variant applies everything again.
Pending(e) == NoGuard \/ ~Applied(e.name)
EndApply == /\ phase = "apply"
            /\ history' = history \o
                 SelectSeq([p \in 1..Len(file) |-> Effect(p, file[p], TRUE)],
                           Pending)
            /\ phase' = "done" /\ Writer /\ UNCHANGED <<file, accepted>>

\* Re-running the end-of-run apply. Credentials are job-local.
Retry == /\ phase = "done" /\ Passes(file) /\ Len(history) <= Lines
         /\ phase' = "apply" /\ credential' = FALSE /\ executes' = FALSE
         /\ UNCHANGED <<file, accepted, history>>

Next == Write \/ InSession \/ Stop \/ EndCheck \/ EndApply \/ Retry
Spec == Init /\ [][Next]_vars

\* Each effect is a request a check accepted, as the run recorded it.
AcceptedOnly == \A n \in 1..Len(history):
                /\ history[n].pos \in accepted
                /\ history[n].kind = file[history[n].pos]
\* No request is applied twice, in the session and at its end together.
AtMostOnce == \A n, m \in 1..Len(history):
              n # m => history[n].pos # history[m].pos
\* All applies of a run together stay within its ceiling.
Ceiling == Len(history) <= Max
CredentialIsolation == ~(credential /\ executes)
=============================================================================
