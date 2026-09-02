/* Minimal RISC-V semihosting test for CH32V307 (flash base 0x08000000).
 * Prints a string via SYS_WRITE0, then exits code 42 via SYS_EXIT_EXTENDED.
 * The slli/ebreak/srai brackets MUST be 32-bit (norvc) for the host to detect. */
    .section .text.start, "ax"
    .global _start
_start:
    /* SYS_WRITE0: a0=0x04, a1 = ptr to NUL-terminated string */
    la      a1, msg
    li      a0, 0x04
    .option push
    .option norvc
    slli    x0, x0, 0x1f
    ebreak
    srai    x0, x0, 7
    .option pop

    /* SYS_EXIT_EXTENDED: a0=0x20, a1 = ptr to [reason, code] */
    la      a1, exitblock
    li      a0, 0x20
    .option push
    .option norvc
    slli    x0, x0, 0x1f
    ebreak
    srai    x0, x0, 7
    .option pop

hang:
    j       hang

    .align 2
exitblock:
    .word   0x20026        /* ADP_Stopped_ApplicationExit */
    .word   42             /* exit code to propagate */
msg:
    .asciz  "hello from semihosting\n"
