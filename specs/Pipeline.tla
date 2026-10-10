----------------------------- MODULE Pipeline -----------------------------
EXTENDS Naturals, Sequences, FiniteSets
\* Constants select negative controls, NOT alternative production designs.
CONSTANTS ByName, RunUntrusted, OverwriteBranch, NoGuard, ForgeableMarker
VARIABLES uploads, latest, accepted, phase, history, branch, credential, executes,
          skipped
vars == <<uploads, latest, accepted, phase, history, branch, credential, executes,
          skipped>>
Ids == 1..2
\* Each upload contains one representative proposal; accepted=0 means no grant.
Init == /\ uploads = [i \in Ids |-> "absent"] /\ latest = 0 /\ accepted = 0
        /\ phase = "check" /\ history = <<>> /\ branch = FALSE
        /\ credential = FALSE /\ executes = FALSE /\ skipped = {}

\* A second attempt may publish the same NAME, but never change an existing ID.
\* Abstracts successful replacement after deletion; a conflicting upload fails
\* without changing state. Checked and producer IDs are separate namespaces;
\* we use the producer index to label the immutable checked copy's contents.
\* A "marked" proposal holds the text apply hides in what it posts.
Upload == \E i \in Ids, kind \in {"comment", "patch", "invalid", "marked"}:
          /\ uploads[i] = "absent" /\ latest' = i
          /\ uploads' = [uploads EXCEPT ![i] = kind]
          /\ UNCHANGED <<accepted, phase, history, branch, credential, executes, skipped>>

\* check.yml: either selector reads untrusted data, collector sanitizes it,
\* Rust check accepts/refuses, and ONLY acceptance uploads checked-outputs.
\* Choosing any existing ID covers ID selection and name selection at check.
\* check refuses a marked proposal; the broken variant lets it through.
Refused(kind) == kind = "invalid" \/ (kind = "marked" /\ ~ForgeableMarker)
Check == \E i \in Ids:
         /\ phase = "check" /\ uploads[i] # "absent"
         /\ accepted' = IF Refused(uploads[i]) THEN 0 ELSE i
         /\ phase' = IF Refused(uploads[i]) THEN "done" ELSE "apply"
         /\ UNCHANGED <<uploads, latest, history, branch, credential, executes, skipped>>

\* apply downloads check's immutable ID, not the current name. The broken
\* variant resolves that name again: the classic check/use substitution.
Selected == IF ByName THEN latest ELSE accepted
\* apply's guard looks on the forge for the name of proposal i, hidden in
\* what it posted. Only the bot's posts count, so what was posted for i is
\* the evidence, and so is a posted proposal that held a name: it is the
\* bot's, and may hold i's.
Evidence(i) == \E n \in 1..Len(history):
               history[n].used = i \/ history[n].kind = "marked"
Apply == /\ phase = "apply" /\ accepted # 0 /\ Len(history) < 2
         /\ LET i == Selected IN
            IF ~NoGuard /\ Evidence(i)
            THEN /\ skipped' = skipped \cup {i}
                 /\ UNCHANGED <<history, branch>>
            ELSE /\ (uploads[i] # "patch" \/ ~branch \/ OverwriteBranch)
                 /\ history' = Append(history, [checked |-> accepted, used |-> i,
                                               kind |-> uploads[i]])
                 /\ branch' = (branch \/ uploads[i] = "patch")
                 /\ UNCHANGED skipped
         /\ phase' = "done" /\ credential' = TRUE
         /\ executes' = RunUntrusted
         /\ UNCHANGED <<uploads, latest, accepted>>

\* Re-running apply reuses the accepted artifact. Re-running check revalidates;
\* there is no persistent consumed-proposal ledger. Credentials are job-local.
Retry == /\ phase = "done" /\ Len(history) < 2
         /\ phase' \in {"check", "apply"}
         /\ credential' = FALSE /\ executes' = FALSE
         /\ UNCHANGED <<uploads, latest, accepted, history, branch, skipped>>

Next == Upload \/ Check \/ Apply \/ Retry
\* [] means "always"; [Next]_vars also permits doing nothing (stuttering).
Spec == Init /\ [][Next]_vars

\* Invariants are predicates TLC evaluates in EVERY reachable state.
\* credential is apply's write credential. The producer's GitHub token is
\* not one: it can only read (run-contract RC-014), so it is not modeled.
AcceptedOnly == \A n \in 1..Len(history): history[n].used = history[n].checked
CredentialIsolation == ~(credential /\ executes)
\* Existing run-named branches block another patch push, not another comment.
BranchOnce == Cardinality({n \in 1..Len(history): history[n].kind = "patch"}) <= 1
\* A re-run of apply posts no accepted proposal a second time.
AtMostOnce == \A i \in Ids:
              Cardinality({n \in 1..Len(history): history[n].used = i}) <= 1
\* A proposal is left out only if it was applied.
NoFalseSkip == \A i \in skipped: \E n \in 1..Len(history): history[n].used = i
=============================================================================
