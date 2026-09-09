// A no-op `___chkstk_darwin`.
//
// Rust emits calls to this stack-probe helper on Apple targets, and linking the
// staticlib for the embedded ones fails with an undefined symbol unless
// something defines it. Probing exists to fault deterministically when a frame
// runs off the end of the stack, so a stub changes nothing for code that stays
// within its stack and turns an overflow into corruption rather than a trap.
// That trade is why this is a workaround and not a design.
//
// build.rs compiles it for iOS, tvOS, visionOS, and watchOS. If a link ever
// fails here with a duplicate symbol rather than a missing one, that means the
// toolchain now provides the real thing for that target and it should come off
// the list in build.rs. Worth rechecking whenever Xcode or the pinned nightly
// moves, because the day every target defines it, this file can go.

.text
.globl ___chkstk_darwin
.p2align 2

___chkstk_darwin:
    ret
