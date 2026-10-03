; Odd and wide integer PHIs exercise both narrowing and widening VM state.
define i64 @phi_widths(i64 %x, i64 %y) noinline {
entry:
  %flag = icmp ult i64 %x, %y
  br i1 %flag, label %left, label %right
left:
  %a1 = trunc i64 %x to i1
  %a8 = trunc i64 %x to i8
  %a17 = trunc i64 %x to i17
  %ax65 = zext i64 %x to i65
  %a65 = or i65 %ax65, 18446744073709551616
  %ax128 = zext i64 %x to i128
  %ay128 = zext i64 %y to i128
  %ah128 = shl i128 %ay128, 64
  %a128 = or i128 %ax128, %ah128
  br label %join
right:
  %b1 = trunc i64 %y to i1
  %b8 = trunc i64 %y to i8
  %b17 = trunc i64 %y to i17
  %by65 = zext i64 %y to i65
  %b65 = sub i65 0, %by65
  %bx128 = zext i64 %x to i128
  %by128 = zext i64 %y to i128
  %bh128 = shl i128 %bx128, 64
  %b128 = or i128 %by128, %bh128
  br label %join
join:
  %p1 = phi i1 [ %a1, %left ], [ %b1, %right ]
  %p8 = phi i8 [ %a8, %left ], [ %b8, %right ]
  %p17 = phi i17 [ %a17, %left ], [ %b17, %right ]
  %p65 = phi i65 [ %a65, %left ], [ %b65, %right ]
  %p128 = phi i128 [ %a128, %left ], [ %b128, %right ]
  %r1 = zext i1 %p1 to i64
  %r8 = zext i8 %p8 to i64
  %r17 = sext i17 %p17 to i64
  %p65h = lshr i65 %p65, 64
  %r65h = trunc i65 %p65h to i64
  %r65l = trunc i65 %p65 to i64
  %p128h = lshr i128 %p128, 64
  %r128h = trunc i128 %p128h to i64
  %r128l = trunc i128 %p128 to i64
  %s1 = add i64 %r1, %r8
  %s2 = xor i64 %s1, %r17
  %s3 = add i64 %s2, %r65h
  %s4 = xor i64 %s3, %r65l
  %s5 = add i64 %s4, %r128h
  %result = xor i64 %s5, %r128l
  ret i64 %result
}

; Poison is legal here: the result select does not observe it on that path.
define i64 @phi_poison_masked(i64 %x, i64 %y) noinline {
entry:
  %flag = icmp eq i64 %x, 0
  br i1 %flag, label %left, label %right
left:
  %bad = add nsw i8 127, 1
  br label %join
right:
  %good = trunc i64 %y to i8
  br label %join
join:
  %value = phi i8 [ %bad, %left ], [ %good, %right ]
  %safe = select i1 %flag, i8 42, i8 %value
  %result = zext i8 %safe to i64
  ret i64 %result
}

define i64 @phi_unreachable(i64 %x, i64 %y) noinline {
entry:
  br label %live
dead:
  br label %join
live:
  %value = xor i64 %x, %y
  br label %join
join:
  %result = phi i64 [ poison, %dead ], [ %value, %live ]
  ret i64 %result
}

define i64 @phi_noninteger(i64 %x, i64 %y) noinline {
entry:
  %px = alloca i64
  %py = alloca i64
  store i64 %x, ptr %px
  store i64 %y, ptr %py
  %flag = icmp ult i64 %x, %y
  br i1 %flag, label %left, label %right
left:
  %vx = insertelement <2 x i64> <i64 0, i64 9>, i64 %x, i32 0
  %fx = uitofp i64 %x to double
  br label %join
right:
  %vy = insertelement <2 x i64> <i64 0, i64 13>, i64 %y, i32 0
  %fy = uitofp i64 %y to double
  br label %join
join:
  %pointer = phi ptr [ %px, %left ], [ %py, %right ]
  %vector = phi <2 x i64> [ %vx, %left ], [ %vy, %right ]
  %floating = phi double [ %fx, %left ], [ %fy, %right ]
  %p = load i64, ptr %pointer
  %v0 = extractelement <2 x i64> %vector, i32 0
  %v1 = extractelement <2 x i64> %vector, i32 1
  %f = bitcast double %floating to i64
  %s1 = add i64 %p, %v0
  %s2 = xor i64 %s1, %v1
  %result = add i64 %s2, %f
  ret i64 %result
}
