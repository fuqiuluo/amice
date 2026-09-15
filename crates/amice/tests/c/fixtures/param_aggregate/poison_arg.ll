; A pointer-based aggregate wrapper reads memory, so a memory(none) call must
; remain untouched. The poison slot models an unused empty C++ object and
; makes a stale memory(none) attribute visibly miscompile the live value.

define internal i64 @return_first(i64 noundef %value, i8 %unused) #0 {
entry:
  ret i64 %value
}

define i32 @main(i32 %argc, ptr %argv) {
entry:
  %value = sext i32 %argc to i64
  %result = call i64 @return_first(i64 noundef %value, i8 poison) #0
  %matches = icmp eq i64 %result, %value
  %exit_code = select i1 %matches, i32 0, i32 1
  ret i32 %exit_code
}

attributes #0 = { memory(none) }
