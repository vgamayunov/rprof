#if !defined(__x86_64__) || !defined(__GNUC__) || __GNUC__ != 13 || defined(__clang__)
#error "rprof-gomp requires GCC 13 on Linux x86_64"
#endif
/* Preserve the entire integer/pointer and SSE argument register bank and the
 * original stack argument area. A returned real target is an ABI-identical
 * tail call, even for an opaque or future signature. rprof_gomp_target retains
 * errno; this bridge does not modify flags before the target executes. */
        .text
        .macro entry name, slot
        .globl \name
        .type \name,@function
\name:
        .cfi_startproc
        endbr64
        subq $184, %rsp
        .cfi_adjust_cfa_offset 184
        movq %rdi, 0(%rsp)
        movq %rsi, 8(%rsp)
        movq %rdx, 16(%rsp)
        movq %rcx, 24(%rsp)
        movq %r8, 32(%rsp)
        movq %r9, 40(%rsp)
        movq %rax, 48(%rsp)
        movdqu %xmm0, 56(%rsp)
        movdqu %xmm1, 72(%rsp)
        movdqu %xmm2, 88(%rsp)
        movdqu %xmm3, 104(%rsp)
        movdqu %xmm4, 120(%rsp)
        movdqu %xmm5, 136(%rsp)
        movdqu %xmm6, 152(%rsp)
        movdqu %xmm7, 168(%rsp)
        movl $\slot, %edi
        call rprof_gomp_target@PLT
        movq %rax, %r11
        movq 0(%rsp), %rdi
        movq 8(%rsp), %rsi
        movq 16(%rsp), %rdx
        movq 24(%rsp), %rcx
        movq 32(%rsp), %r8
        movq 40(%rsp), %r9
        movq 48(%rsp), %rax
        movdqu 56(%rsp), %xmm0
        movdqu 72(%rsp), %xmm1
        movdqu 88(%rsp), %xmm2
        movdqu 104(%rsp), %xmm3
        movdqu 120(%rsp), %xmm4
        movdqu 136(%rsp), %xmm5
        movdqu 152(%rsp), %xmm6
        movdqu 168(%rsp), %xmm7
        addq $184, %rsp
        .cfi_adjust_cfa_offset -184
        jmp *%r11
        .cfi_endproc
        .size \name,.-\name
        .endm
        .set rprof_slot,0
#define X(name, wrapped) entry name, rprof_slot ; .set rprof_slot,rprof_slot+1
#include "symbols.def"
#undef X
        .section .note.GNU-stack,"",@progbits
