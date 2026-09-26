; Keep the boolean operations in hand-written IR: a C frontend may widen or
; simplify them before MBA runs. Only the annotated functions should change.
define i1 @boolean_gate(i1 %a, i1 %b) {
entry:
  %value = or i1 %a, %b
  ret i1 %value
}

define i1 @boolean_xor(i1 %a, i1 %b) {
entry:
  %value = xor i1 %a, %b
  ret i1 %value
}

define i1 @boolean_add(i1 %a, i1 %b) {
entry:
  %value = add i1 %a, %b
  ret i1 %value
}

define i1 @boolean_sub(i1 %a, i1 %b) {
entry:
  %value = sub i1 %a, %b
  ret i1 %value
}

define i1 @boolean_rhs(i1 %a) {
entry:
  %value = xor i1 %a, true
  ret i1 %value
}

define i1 @boolean_lhs(i1 %a) {
entry:
  %value = sub i1 true, %a
  ret i1 %value
}

define i1 @constant_or() {
entry:
  %value = or i1 false, true
  ret i1 %value
}

define i1 @constant_xor() {
entry:
  %value = xor i1 true, true
  ret i1 %value
}

define i1 @constant_add() {
entry:
  %value = add i1 true, true
  ret i1 %value
}

define i1 @constant_sub() {
entry:
  %value = sub i1 false, true
  ret i1 %value
}

define i8 @wide_constant() {
entry:
  %value = add i8 250, 6
  ret i8 %value
}

; Exhaust all four input pairs. Return nonzero if any rewritten result differs
; from the original operation, including i1 arithmetic overflow/underflow.
define i32 @main() {
entry:
  br label %loop

loop:
  %pair = phi i32 [ 0, %entry ], [ %next, %check ]
  %a = trunc i32 %pair to i1
  %high = lshr i32 %pair, 1
  %b = trunc i32 %high to i1
  %expected_or = or i1 %a, %b
  %expected_xor = xor i1 %a, %b
  %expected_not = xor i1 %a, true
  %r0 = call i1 @boolean_gate(i1 %a, i1 %b)
  %r1 = call i1 @boolean_xor(i1 %a, i1 %b)
  %r2 = call i1 @boolean_add(i1 %a, i1 %b)
  %r3 = call i1 @boolean_sub(i1 %a, i1 %b)
  %r4 = call i1 @boolean_rhs(i1 %a)
  %r5 = call i1 @boolean_lhs(i1 %a)
  %r6 = call i1 @constant_or()
  %r7 = call i1 @constant_xor()
  %r8 = call i1 @constant_add()
  %r9 = call i1 @constant_sub()
  %r10 = call i8 @wide_constant()
  %e0 = icmp ne i1 %r0, %expected_or
  %e1 = icmp ne i1 %r1, %expected_xor
  %e2 = icmp ne i1 %r2, %expected_xor
  %e3 = icmp ne i1 %r3, %expected_xor
  %e4 = icmp ne i1 %r4, %expected_not
  %e5 = icmp ne i1 %r5, %expected_not
  %e6 = icmp ne i1 %r6, true
  %e7 = icmp ne i1 %r7, false
  %e8 = icmp ne i1 %r8, false
  %e9 = icmp ne i1 %r9, true
  %e10 = icmp ne i8 %r10, 0
  %errors1 = or i1 %e0, %e1
  %errors2 = or i1 %errors1, %e2
  %errors3 = or i1 %errors2, %e3
  %errors4 = or i1 %errors3, %e4
  %errors5 = or i1 %errors4, %e5
  %errors6 = or i1 %errors5, %e6
  %errors7 = or i1 %errors6, %e7
  %errors8 = or i1 %errors7, %e8
  %errors9 = or i1 %errors8, %e9
  %errors10 = or i1 %errors9, %e10
  br i1 %errors10, label %fail, label %check

check:
  %next = add i32 %pair, 1
  %done = icmp eq i32 %next, 4
  br i1 %done, label %pass, label %loop

pass:
  ret i32 0

fail:
  ret i32 1
}

@mba_annotation = private constant [5 x i8] c"+mba\00", section "llvm.metadata"
@llvm.global.annotations = appending global [11 x { ptr, ptr, ptr, i32, ptr }] [
  { ptr, ptr, ptr, i32, ptr } { ptr @boolean_gate, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @boolean_xor, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @boolean_add, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @boolean_sub, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @boolean_rhs, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @boolean_lhs, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @constant_or, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @constant_xor, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @constant_add, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @constant_sub, ptr @mba_annotation, ptr null, i32 0, ptr null },
  { ptr, ptr, ptr, i32, ptr } { ptr @wide_constant, ptr @mba_annotation, ptr null, i32 0, ptr null }
], section "llvm.metadata"
