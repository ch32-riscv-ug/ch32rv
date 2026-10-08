/* UART-free bare-metal startup for the V205/X315 host transport tests. */
    .section .text.start,"ax"
    .global _start
    .option norvc
_start:
    j reset
reset:
    csrw mstatus, zero
    li sp, 0x20007ff0
    /* V205 resets through the flash alias at 0; X315 may enter at 0x08000000.
     * Absolute addresses keep RAM/data initialization correct in either window. */
    lui t0, %hi(__data_load)
    addi t0, t0, %lo(__data_load)
    lui t1, %hi(__data_start)
    addi t1, t1, %lo(__data_start)
    lui t2, %hi(__data_end)
    addi t2, t2, %lo(__data_end)
1:  bgeu t1, t2, 2f
    lw a0, 0(t0)
    sw a0, 0(t1)
    addi t0, t0, 4
    addi t1, t1, 4
    j 1b
2:  lui t1, %hi(__bss_start)
    addi t1, t1, %lo(__bss_start)
    lui t2, %hi(__bss_end)
    addi t2, t2, %lo(__bss_end)
3:  bgeu t1, t2, 4f
    sw zero, 0(t1)
    addi t1, t1, 4
    j 3b
4:  call main
5:  j 5b
