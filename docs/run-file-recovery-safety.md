# Run file recovery

Run file tracking covers live filesystem operations exposed by the runtime.
Program launches, process-capable Tools, and MCP calls set a durable coverage
warning because their writes can bypass these hooks. A Run can therefore have
zero tracked paths while still reporting possible untracked writes.

Each new recovery record keeps the first before-image, its entry kind, and
available security metadata within the configured capture budgets. Successful
tracked operations also supply an immediate post-image. Before Run completion,
the daemon freezes a versioned hash, size, type, and metadata token only when
the current state still matches that post-image. Pending background work,
interrupted capture, unsupported paths, or a change before completion produce
an explicit read-only reason. Historical missing proof is never reconstructed
from the current filesystem.

Listing and rewind validate that both the Run and its snapshot rows belong to
the requested live Session. Rewind serializes admission and filesystem commits,
and refuses while any Session in the profile has an active Run or pending or
background Invocation. This is deliberately broader than overlap detection:
opaque programs and MCP servers do not provide reliable path access sets.

Regular-file recovery compares the frozen after-state, uses no-follow anchored
I/O, and preserves supported before-image metadata. Whole-file images retain
the existing 2 MiB limit; interactive hunk review remains limited to 256 KiB.
Before changing bytes, rewind persists a private bounded recovery receipt and
audit entry. It verifies the resulting image and marks the receipt applied or
ambiguous. Ambiguous outcomes are never retried by overwriting the path.

Directory creation is recorded only after creation of that exact directory
succeeds. Rewind removes only the same positively recorded directory and only
when it is empty. It never recursively deletes a directory or follows a changed
file into a directory or symlink. Requested symlink or alias routes are listed
with a specific read-only reason rather than being silently treated as regular
paths. Windows namespace recovery additionally requires live retained-object
authority; hashes and database existence flags alone do not authorize it.

The daemon's locks exclude cooperating work in this profile. They cannot make
the final filesystem comparison and mutation transactional against a separate,
noncooperating external writer. Anchored operations retain the existing review
I/O limitation for such races. No force override bypasses these checks.

Historical listings load at most one bounded snapshot body at a time. Diff
generation limits input size and aggregate response size, with explicit omission
messages. Expiry and successful rewind retain path tombstones, so selecting the
latest Run cannot silently fall back to older recovery data.
