define i64 @multiple_outputs(i64 %x, i64 %y) noinline {
  %a = add i64 %x, %y
  %b = sub i64 %a, %x
  %c = xor i64 %b, %a
  %r = mul i64 %a, %c
  ret i64 %r
}
define i64 @multiple_phis(i64 %x, i64 %y) noinline {
entry:
  %c = icmp ult i64 %x, %y
  br i1 %c, label %left, label %right
left:
  %a = add i64 %x, %y
  %b = xor i64 %a, %x
  br label %join
right:
  %d = sub i64 %x, %y
  %e = or i64 %d, %y
  br label %join
join:
  %p = phi i64 [%a, %left], [%d, %right]
  %q = phi i64 [%b, %left], [%e, %right]
  %r = mul i64 %p, %q
  ret i64 %r
}
define i64 @masked_poison(i64 %x, i64 %y) noinline {
  %a = add i64 %x, poison
  %b = xor i64 %a, %y
  %r = select i1 false, i64 %b, i64 %x
  ret i64 %r
}
define i64 @masked_undef(i64 %x, i64 %y) noinline {
  %a = and i64 undef, 0
  %b = add i64 %a, %x
  %c = xor i64 %b, %y
  ret i64 %c
}
define i64 @masked_overflow(i64 %x, i64 %y) noinline {
  %a = add nuw i64 %x, %y
  %b = xor i64 %a, %x
  %sum = add i64 %x, %y
  %overflow = icmp ult i64 %sum, %x
  %r = select i1 %overflow, i64 %x, i64 %b
  ret i64 %r
}
define i64 @branched_dag(i64 %x, i64 %y) noinline {
  %a = add i64 %x, %y
  %b = sub i64 %a, %x
  %c = xor i64 %a, %y
  %d = or i64 %b, %c
  %e = and i64 %d, %a
  %f = sub i64 %e, %c
  %g = add i64 %f, %b
  %h = xor i64 %g, %e
  ret i64 %h
}
define i64 @original_loop(i64 %x, i64 %y) noinline {
entry:
  br label %loop
loop:
  %i = phi i64 [0, %entry], [%next, %loop]
  %a = phi i64 [%x, %entry], [%b, %loop]
  %b = add i64 %a, %y
  %next = add i64 %i, 1
  %c = icmp ult i64 %next, 4
  br i1 %c, label %loop, label %exit
exit:
  ret i64 %b
}
define i64 @multi_region(i64 %x, i64 %y) noinline {
  %a = add i64 %x, %y
  %b = xor i64 %a, %x
  %c = mul i64 %b, %y
  %d = sub i64 %c, %a
  %e = or i64 %d, %b
  ret i64 %e
}
