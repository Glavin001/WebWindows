/* Multiboot entry: a stack, main, then report and halt. */
    .section .multiboot, "a"
    .align 4
    .long 0x1BADB002
    .long 0
    .long -0x1BADB002
    .text
    .globl _start
_start:
    mov $stack_top, %esp
    call main
    push $done
    call ee_printf
1:  cli
    hlt
    jmp 1b
    .section .rodata
done: .asciz "@@END\n"
    .bss
    .align 16
    .space 1 << 20
stack_top:
