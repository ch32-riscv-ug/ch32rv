/* ch32rv RAM loader: erase and program fast pages of CH32 code flash (docs/oep-host.ja.md §5.1).
 *
 * One binary for every family: only RV32EC instructions and x1..x15, so it runs on the RV32EC
 * parts (V003 / V00x) and on every RV32IMAC one; position independent (relative branches only).
 * The three programming mechanisms are the ones DebugModule::flash_program_page drives over DMI
 * and that were verified on hardware:
 *   a3 = 0  buffered : BUFRST, per word store + BUFLOAD, then STRT   (V003, V00x, X035, L103)
 *   a3 = 1  pgstart  : FTPG, per word store (wait WRBSY), then PGSTART (V20x, V30x)
 *   a3 = 2  v103     : PG, halfword stores, then the V103 commit        (V103)
 *   a3 = 3  x315     : PER erase (4 KiB), sixteen 256-byte PGSTART commits (X315)
 *
 * In:  a0 = first page address (aligned to a2), a1 = source buffer in RAM (a5 pages back to back),
 *      a2 = page bytes (the fast page size: 64 / 128 / 256 / 4096), a3 = mode, a5 = page count (>= 1).
 *      a4 is set here to the FLASH base (0x40022000), one register less in the run request so it
 *      fits a probe's smallest frame (64 bytes). FLASH already unlocked, fast mode included (KEYR +
 *      MODEKEYR), and mstatus.MIE clear - the host does both.
 * Out: a0 = 0, or 0x80000000 | STATR when the controller reported a write-protect error (the
 *      pages before it are written).
 * Stop: always at the ebreak at offset 4 (`done`), so the host checks dpc == base + 4 and a0.
 * A controller that never clears BSY leaves the loader spinning; the host's run timeout halts it.
 * Registers: s0 / s1 walk the page's destination / source, ra marks the end of its source.
 */
    .equ STATR,   0x0C
    .equ CTLR,    0x10
    .equ ADDR,    0x14
    .equ MAGIC,   0x34        /* CH32V103: the commit register */
    .equ BSY,     0x01
    .equ WRBSY,   0x02
    .equ WRPRTERR, 0x10

    /* Wait until STATR & \mask == 0; leaves STATR in t2. */
    .macro WAIT mask
1:  lw      t2, STATR(a4)
    andi    t1, t2, \mask
    bnez    t1, 1b
    .endm

    /* CH32V103 commit: *(FLASH + 0x34) = *((a0 & ~3) ^ 0x1000). Clobbers t0, t1. */
    .macro COMMIT
    andi    t0, a0, -4
    li      t1, 0x1000
    xor     t0, t0, t1
    lw      t0, 0(t0)
    sw      t0, MAGIC(a4)
    .endm

    .text
    .globl  _start
    .option norvc
_start:
    j       main
done:
    ebreak
    .option rvc

main:
    lui     a4, 0x40022         /* FLASH base */
page:
    /* ---- erase the page: FTER, ADDR, FTER|STRT ---- */
    lui     t0, 0x20            /* FTER = 1 << 17 */
    li      t1, 3
    bne     a3, t1, 6f
    li      t0, 2               /* X315 PER = 1 << 1, 4 KiB */
6:
    sw      t0, CTLR(a4)
    sw      a0, ADDR(a4)
    addi    t0, t0, 0x40        /* | STRT */
    sw      t0, CTLR(a4)
    WAIT    BSY
    sw      zero, CTLR(a4)
    andi    t1, t2, WRPRTERR
    bnez    t1, fail
    sw      t2, STATR(a4)       /* write the flags back to clear them (EOP) */
    li      t0, 2
    bne     a3, t0, 2f
    COMMIT                      /* V103: the commit follows the erase too */
2:
    mv      s0, a0              /* destination */
    mv      s1, a1              /* source */
    add     ra, a1, a2          /* source end */
    beqz    a3, buffered
    li      t0, 1
    beq     a3, t0, pgstart
    li      t0, 3
    beq     a3, t0, pgstart

/* ---- CH32V103: standard halfword programming ---- */
v103:
    li      t0, 1               /* PG */
    sw      t0, CTLR(a4)
3:  lhu     t0, 0(s1)
    sh      t0, 0(s0)
    WAIT    BSY
    addi    s0, s0, 2
    addi    s1, s1, 2
    bltu    s1, ra, 3b
    sw      zero, CTLR(a4)
    COMMIT
    j       finish

/* ---- V003 / V00x / X035 / L103: buffered fast page ---- */
buffered:
    lui     t0, 0x10            /* FTPG = 1 << 16 */
    sw      t0, CTLR(a4)
    lui     t0, 0x90            /* FTPG | BUFRST (1 << 19) */
    sw      t0, CTLR(a4)
    WAIT    BSY
4:  lw      t2, 0(s1)
    sw      t2, 0(s0)
    lui     t0, 0x50            /* FTPG | BUFLOAD (1 << 18) */
    sw      t0, CTLR(a4)
    WAIT    BSY
    addi    s0, s0, 4
    addi    s1, s1, 4
    bltu    s1, ra, 4b
    sw      a0, ADDR(a4)
    lui     t0, 0x10
    addi    t0, t0, 0x40        /* FTPG | STRT */
    sw      t0, CTLR(a4)
    WAIT    BSY
    j       finish

/* ---- V20x / V30x: fast page, then PGSTART ---- */
pgstart:
    li      t0, 3
    bne     a3, t0, 7f
    addi    ra, s1, 256         /* X315 commits one 256-byte program page at a time */
7:
    lui     t0, 0x10            /* FTPG */
    sw      t0, CTLR(a4)
5:  lw      t2, 0(s1)
    sw      t2, 0(s0)
    WAIT    WRBSY
    addi    s0, s0, 4
    addi    s1, s1, 4
    bltu    s1, ra, 5b
    lui     t0, 0x210           /* FTPG | PGSTART (1 << 21) */
    sw      t0, CTLR(a4)
    WAIT    BSY
    li      t0, 3
    bne     a3, t0, finish
    sw      zero, CTLR(a4)
    sub     t0, s0, a0
    bltu    t0, a2, pgstart

finish:
    lw      t2, STATR(a4)
    sw      zero, CTLR(a4)
    andi    t1, t2, WRPRTERR
    bnez    t1, fail
    sw      t2, STATR(a4)
    /* next page */
    add     a0, a0, a2
    add     a1, a1, a2
    addi    a5, a5, -1
    bnez    a5, page
    li      a0, 0
    j       done

fail:
    sw      zero, CTLR(a4)
    sw      t2, STATR(a4)
    lui     a0, 0x80000
    or      a0, a0, t2
    j       done
