@address = global ptr blockaddress(@address_taken, %target)
declare i32 @personality(...)

define i64 @eh_function(i64 %x, i64 %y) personality ptr @personality {
  %a = add i64 %x, %y
  %b = xor i64 %a, %x
  ret i64 %b
}
define i64 @address_taken(i64 %x, i64 %y) {
entry:
  br label %target
target:
  %a = add i64 %x, %y
  %b = xor i64 %a, %x
  ret i64 %b
}
define i128 @wide(i128 %x, i128 %y) {
  %a = add i128 %x, %y
  %b = xor i128 %a, %x
  ret i128 %b
}
define <2 x i32> @vector(<2 x i32> %x, <2 x i32> %y) {
  %a = add <2 x i32> %x, %y
  %b = xor <2 x i32> %a, %x
  ret <2 x i32> %b
}
define i64 @disconnected(i64 %x, i64 %y) {
  %a = add i64 %x, %y
  %b = xor i64 %x, %y
  %r = mul i64 %a, %b
  ret i64 %r
}
define i64 @store_boundary(i64 %x, i64 %y, ptr %out) {
  %a = add i64 %x, %y
  store i64 %a, ptr %out
  %b = xor i64 %a, %x
  ret i64 %b
}
define i64 @constants() {
  %a = add i64 9, 12
  %b = xor i64 %a, 7
  ret i64 %b
}
define i64 @unreachable_block(i64 %x, i64 %y) {
entry:
  ret i64 %x
dead:
  %a = add i64 %x, %y
  %b = xor i64 %a, %x
  ret i64 %b
}
; Two entries into a cycle: natural-loop analysis alone misses this SCC.
define i64 @irreducible(i64 %x, i64 %y, i1 %choice, i1 %again) {
entry:
  br i1 %choice, label %left, label %right
left:
  %a = add i64 %x, %y
  %b = xor i64 %a, %x
  br label %right
right:
  %c = add i64 %x, %y
  %d = xor i64 %c, %y
  br i1 %again, label %left, label %exit
exit:
  ret i64 %d
}
