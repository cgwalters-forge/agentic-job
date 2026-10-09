----------------------------- MODULE Pipeline -----------------------------
EXTENDS Naturals, Sequences, FiniteSets
\* Constants select negative controls, NOT alternative production designs.
CONSTANTS ByName, RunUntrusted, OverwriteBranch
VARIABLES uploads, latest, accepted, phase, history, branch, credential, executes
vars == <<uploads, latest, accepted, phase, history, branch, credential, executes>>
Ids == 1..2
\* Each upload contains one representative proposal; accepted=0 means no grant.
Init == /\ uploads = [i \in Ids |-> "absent"] /\ latest = 0 /\ accepted = 0
        /\ phase = "check" /\ history = <<>> /\ branch = FALSE
        /\ credential = FALSE /\ executes = FALSE

\* A second attempt may publish the same NAME, but never change an existing ID.
\* Abstracts successful replacement after deletion; a conflicting upload fails
\* without changing state. Checked and producer IDs are separate namespaces;
\* we use the producer index to label the immutable checked copy's contents.
Upload == \E i \in Ids, kind \in {"comment", "patch", "invalid"}:
          /\ uploads[i] = "absent" /\ latest' = i
          /\ uploads' = [uploads EXCEPT ![i] = kind]
          /\ UNCHANGED <<accepted, phase, history, branch, credential, executes>>

\* check.yml: either selector reads untrusted data, collector sanitizes it,
\* Rust check accepts/refuses, and ONLY acceptance uploads checked-outputs.
\* Choosing any existing ID covers ID selection and name selection at check.
Check == \E i \in Ids:
         /\ phase = "check" /\ uploads[i] # "absent"
         /\ accepted' = IF uploads[i] = "invalid" THEN 0 ELSE i
         /\ phase' = IF uploads[i] = "invalid" THEN "done" ELSE "apply"
         /\ UNCHANGED <<uploads, latest, history, branch, credential, executes>>

\* apply downloads check's immutable ID, not the current name. The broken
\* variant resolves that name again: the classic check/use substitution.
Selected == IF ByName THEN latest ELSE accepted
Apply == /\ phase = "apply" /\ accepted # 0 /\ Len(history) < 2
         /\ LET i == Selected IN
            /\ (uploads[i] # "patch" \/ ~branch \/ OverwriteBranch)
            /\ history' = Append(history, [checked |-> accepted, used |-> i,
                                          kind |-> uploads[i]])
            /\ branch' = (branch \/ uploads[i] = "patch")
         /\ phase' = "done" /\ credential' = TRUE
         /\ executes' = RunUntrusted
         /\ UNCHANGED <<uploads, latest, accepted>>

\* Re-running apply reuses the accepted artifact. Re-running check revalidates;
\* there is no persistent consumed-proposal ledger. Credentials are job-local.
Retry == /\ phase = "done" /\ Len(history) < 2
         /\ phase' \in {"check", "apply"}
         /\ credential' = FALSE /\ executes' = FALSE
         /\ UNCHANGED <<uploads, latest, accepted, history, branch>>

Next == Upload \/ Check \/ Apply \/ Retry
\* [] means "always"; [Next]_vars also permits doing nothing (stuttering).
Spec == Init /\ [][Next]_vars

\* Invariants are predicates TLC evaluates in EVERY reachable state.
AcceptedOnly == \A n \in 1..Len(history): history[n].used = history[n].checked
CredentialIsolation == ~(credential /\ executes)
\* Existing run-named branches block another patch push, not another comment.
BranchOnce == Cardinality({n \in 1..Len(history): history[n].kind = "patch"}) <= 1
\* This desired property is FALSE today: a comment-only apply can be re-run.
AtMostOnce == \A i \in Ids:
              Cardinality({n \in 1..Len(history): history[n].used = i}) <= 1
=============================================================================
