; The caller supplies all inputs at runtime. Blocks intentionally are not in
; dominance order, and the loop carries both arithmetic and boolean state.
declare i32 @llvm.fshl.i32(i32, i32, i32)

define i64 @region_mutual_loop(i64 %xx, i64 %yy, i64 %nn, i64 %ss) noinline {
entry:
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %n = and i64 %nn, 1023
  %salt = trunc i64 %ss to i32
  br label %loop
loop:
  %a = phi i32 [ %x, %entry ], [ %next_b, %loop ]
  %b = phi i32 [ %y, %entry ], [ %next_a, %loop ]
  %i = phi i64 [ 0, %entry ], [ %next_i, %loop ]
  %sum = add i32 %a, %b
  %next_a = xor i32 %sum, %salt
  %next_b = sub i32 %a, %salt
  %next_i = add i64 %i, 1
  %done = icmp uge i64 %i, %n
  br i1 %done, label %exit, label %loop
exit:
  %result = xor i32 %next_a, %next_b
  %wide = zext i32 %result to i64
  ret i64 %wide
}

define i64 @region_poison_select(i64 %xx, i64 %yy, i64 %zz, i64 %unused) noinline {
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %c = trunc i64 %zz to i1
  %p = add i32 poison, %y
  %a = select i1 %c, i32 %p, i32 %x
  %b = add i32 %a, %y
  %safe = select i1 %c, i32 42, i32 %b
  %r = zext i32 %safe to i64
  ret i64 %r
}

define i64 @region_undef_mask(i64 %xx, i64 %yy, i64 %zz, i64 %unused) noinline {
  %x = trunc i64 %xx to i32
  %u = add i32 undef, 0
  %zero = and i32 %u, 0
  %r = add i32 %zero, %x
  %wide = zext i32 %r to i64
  ret i64 %wide
}

define i64 @region_poison_rhs(i64 %xx, i64 %yy, i64 %zz, i64 %unused) noinline {
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %c = trunc i64 %zz to i1
  %p = add i32 %x, poison
  %a = select i1 %c, i32 %p, i32 %x
  %b = add i32 %a, %y
  %safe = select i1 %c, i32 42, i32 %b
  %r = zext i32 %safe to i64
  ret i64 %r
}

define i64 @region_poison_condition(i64 %xx, i64 %yy, i64 %zz, i64 %unused) noinline {
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %c = trunc i64 %zz to i1
  %p = select i1 poison, i32 %x, i32 %y
  %a = select i1 %c, i32 %p, i32 %x
  %b = add i32 %a, %y
  %safe = select i1 %c, i32 42, i32 %b
  %r = zext i32 %safe to i64
  ret i64 %r
}

define i64 @region_flags(i64 %xx, i64 %yy, i64 %zz, i64 %unused) noinline {
  %x0 = trunc i64 %xx to i32
  %y0 = trunc i64 %yy to i32
  %x = and i32 %x0, 1073741823
  %y = and i32 %y0, 1073741823
  %s = add nuw nsw i32 %x, %y
  %d = sub nuw nsw i32 %s, %x
  %r = zext i32 %d to i64
  ret i64 %r
}

define i64 @region_memory_intrinsic(i64 %xx, i64 %yy, i64 %zz, i64 %unused) noinline {
  %slot = alloca i32, align 4
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %s = add i32 %x, %y
  store volatile i32 %s, ptr %slot, align 4
  %old = atomicrmw xor ptr %slot, i32 %y seq_cst, align 4
  %new = load volatile i32, ptr %slot, align 4
  %rot = call i32 @llvm.fshl.i32(i32 %new, i32 %old, i32 7)
  %r = sub i32 %rot, %old
  %wide = zext i32 %r to i64
  ret i64 %wide
}

define i64 @region_irreducible(i64 %xx, i64 %yy, i64 %zz, i64 %ss) noinline {
entry:
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %z = trunc i64 %zz to i32
  %c = trunc i64 %ss to i1
  br i1 %c, label %a, label %b
a:
  %pa = phi i32 [ %x, %entry ], [ %vb, %b ]
  %ia = phi i32 [ 0, %entry ], [ %nb, %b ]
  %va = add i32 %pa, %y
  %na = add i32 %ia, 1
  %enda = icmp uge i32 %na, 7
  br i1 %enda, label %exit_a, label %b
b:
  %pb = phi i32 [ %z, %entry ], [ %va, %a ]
  %ib = phi i32 [ 0, %entry ], [ %na, %a ]
  %vb = sub i32 %pb, %y
  %nb = add i32 %ib, 1
  %endb = icmp uge i32 %nb, 9
  br i1 %endb, label %exit_b, label %a
exit_a:
  %ra = zext i32 %va to i64
  ret i64 %ra
exit_b:
  %rb = zext i32 %vb to i64
  ret i64 %rb
}

define i64 @undef_and(i64 %x, i64 %y, i64 %z, i64 %unused) noinline {
  %r = and i64 undef, 0
  ret i64 %r
}

define i64 @undef_or(i64 %x, i64 %y, i64 %z, i64 %unused) noinline {
  %r = or i64 undef, -1
  ret i64 %r
}

define i64 @region_branch(i64 %xx, i64 %yy, i64 %zz, i64 %cc) noinline {
entry:
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %z = trunc i64 %zz to i32
  %c = trunc i64 %cc to i1
  br i1 %c, label %left, label %right
join:
  %p = phi i32 [ %a, %left ], [ %b, %right ]
  %s = select i1 %c, i32 %p, i32 %z
  %r = sub i32 %s, %x
  %wide = zext i32 %r to i64
  ret i64 %wide
right:
  %b = sub i32 %x, %y
  br label %join
left:
  %a = add i32 %x, %y
  br label %join
}

define i64 @region_loop(i64 %xx, i64 %yy, i64 %nn, i64 %ss) noinline {
entry:
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %salt = trunc i64 %ss to i32
  %n = and i64 %nn, 255
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %body ]
  %state = phi i32 [ %x, %entry ], [ %mixed, %body ]
  %gate = phi i1 [ true, %entry ], [ %flip, %body ]
  %done = icmp eq i64 %i, %n
  br i1 %done, label %exit, label %body
body:
  %sum = add i32 %state, %y
  %diff = sub i32 %sum, %salt
  %choice = select i1 %gate, i32 %sum, i32 %diff
  %mixed = xor i32 %choice, %salt
  %flip = xor i1 %gate, true
  %next = add i64 %i, 1
  br label %loop
exit:
  %wide = zext i32 %state to i64
  ret i64 %wide
}

; The i1 result is itself the condition of another selected node.
define i64 @region_switch(i64 %xx, i64 %yy, i64 %cc, i64 %unused) noinline {
entry:
  %x = trunc i64 %xx to i32
  %y = trunc i64 %yy to i32
  %c = trunc i64 %cc to i2
  switch i2 %c, label %join [ i2 0, label %join
                            i2 1, label %other ]
other:
  %a = add i32 %x, %y
  br label %join
join:
  %p = phi i32 [ %x, %entry ], [ %x, %entry ], [ %a, %other ]
  %r = sub i32 %p, %y
  %wide = zext i32 %r to i64
  ret i64 %wide
}

define i64 @region_condition(i64 %xx, i64 %yy, i64 %zz, i64 %unused) noinline {
  %x = trunc i64 %xx to i1
  %y = trunc i64 %yy to i1
  %z = trunc i64 %zz to i1
  %c = add i1 %x, %y
  %v = select i1 %c, i1 %y, i1 %z
  %r = sub i1 %v, %x
  %wide = zext i1 %r to i64
  ret i64 %wide
}

define i64 @strict_integer(i64 %x, i64 %y, i64 %z, i64 %unused) strictfp noinline {
  %a = trunc i64 %x to i32
  %b = trunc i64 %y to i32
  %sum = add i32 %a, %b
  %r = zext i32 %sum to i64
  ret i64 %r
}

define i64 @soft_integer(i64 %x, i64 %y, i64 %z, i64 %unused) noinline "use-soft-float"="true" {
  %a = trunc i64 %x to i32
  %b = trunc i64 %y to i32
  %sum = add i32 %a, %b
  %r = zext i32 %sum to i64
  ret i64 %r
}
