define i64 @diamond(i64 %x, i64 %y) noinline {
entry:
  %cond = icmp slt i64 %x, %y
  br i1 %cond, label %left, label %right
left:
  %l = add i64 %x, %y
  br label %join
right:
  %r = xor i64 %x, %y
  br label %join
join:
  %v = phi i64 [ %l, %left ], [ %r, %right ]
  %w = phi i64 [ %y, %left ], [ %x, %right ]
  %result = mul i64 %v, %w
  ret i64 %result
}

define i64 @swap_loop(i64 %x, i64 %y) noinline {
entry:
  %limit = and i64 %x, 31
  br label %loop
loop:
  %n = phi i64 [ 0, %entry ], [ %next, %loop ]
  %a = phi i64 [ %x, %entry ], [ %b, %loop ]
  %b = phi i64 [ %y, %entry ], [ %a, %loop ]
  %sum = phi i64 [ 0, %entry ], [ %sum.next, %loop ]
  %sum.next = add i64 %sum, %a
  %next = add i64 %n, 1
  %again = icmp ult i64 %n, %limit
  br i1 %again, label %loop, label %exit
exit:
  %result = xor i64 %sum.next, %b
  ret i64 %result
}

define i64 @duplicate_switch(i64 %x, i64 %y) noinline {
entry:
  switch i64 %x, label %join [ i64 0, label %join
                              i64 1, label %join
                              i64 -1, label %other ]
other:
  %extra = add i64 %y, 7
  br label %join
join:
  %v = phi i64 [ %y, %entry ], [ %y, %entry ], [ %y, %entry ], [ %extra, %other ]
  %result = xor i64 %v, %x
  ret i64 %result
}

define i64 @duplicate_branch(i64 %x, i64 %y) noinline {
entry:
  %cond = icmp eq i64 %x, 0
  br i1 %cond, label %join, label %join
join:
  %v = phi i64 [ %y, %entry ], [ %y, %entry ]
  %result = add i64 %v, %x
  ret i64 %result
}

define i64 @wide_switch(i64 %x, i64 %y) noinline {
entry:
  %lo = zext i64 %x to i128
  %hi = zext i64 %y to i128
  %high = shl i128 %hi, 64
  %condition = or i128 %high, %lo
  switch i128 %condition, label %other [ i128 0, label %zero
                                       i128 -1, label %ones
                                       i128 18446744073709551616, label %one_high ]
zero:
  ret i64 13
ones:
  ret i64 17
one_high:
  ret i64 19
other:
  %result = add i64 %x, %y
  ret i64 %result
}

define i64 @aggregate_phi(i64 %x, i64 %y) noinline {
entry:
  %cond = icmp eq i64 %x, 0
  br i1 %cond, label %left, label %right
left:
  %a = insertvalue { i64, i64 } { i64 7, i64 0 }, i64 %y, 1
  br label %join
right:
  %b = insertvalue { i64, i64 } { i64 0, i64 3 }, i64 %x, 0
  br label %join
join:
  %pair = phi { i64, i64 } [ %a, %left ], [ %b, %right ]
  %p = extractvalue { i64, i64 } %pair, 0
  %q = extractvalue { i64, i64 } %pair, 1
  %result = xor i64 %p, %q
  ret i64 %result
}

define i64 @recursive(i64 %x, i64 %y) noinline {
entry:
  %n = and i64 %x, 15
  %done = icmp eq i64 %n, 0
  br i1 %done, label %base, label %recurse
base:
  ret i64 %y
recurse:
  %next = sub i64 %n, 1
  %y.next = add i64 %y, 2
  %value = call i64 @recursive(i64 %next, i64 %y.next)
  %result = xor i64 %value, %n
  ret i64 %result
}
