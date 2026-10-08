/* Standalone test of the host transports, independent of Arduino and UART.
 * CHANNEL: 0=dmdata/SDI, 1=dmseq, 2=RTT. Results at 0x20001000:
 * magic, iterations, RAM-check failures, last received byte, received count. */
#include <stdint.h>
#ifndef CHANNEL
#define CHANNEL 0
#endif
static volatile uint32_t *const result = (void *)0x20001000;
static volatile uint32_t *const d0 = (void *)0xe0000340;
static volatile uint32_t *const d1 = (void *)0xe0000344;
static volatile uint32_t ram_check[32];

struct Ring {
    const char *name;
    char *buffer;
    uint32_t size;
    volatile uint32_t wr, rd;
    uint32_t flags;
};
struct Rtt {
    char id[16];
    uint32_t up_count, down_count;
    struct Ring up, down;
};
static char up_buf[128], down_buf[64];
struct Rtt _SEGGER_RTT = {
    "SEGGER RTT", 1, 1,
    {"up", up_buf, sizeof up_buf, 0, 0, 0},
    {"down", down_buf, sizeof down_buf, 0, 0, 0}
};

static void received(uint8_t c) {
    result[3] = c;
    result[4]++;
}
static uint8_t crc8(const uint8_t *b, unsigned n) {
    uint8_t crc = 255;
    while (n--) {
        crc ^= *b++;
        for (unsigned i = 0; i < 8; i++) crc = (crc & 128) ? (crc << 1) ^ 7 : crc << 1;
    }
    return crc;
}
static void spin(void) { result[1]++; }
static void dmdata(void) {
    *d0 = 0;
    for (;;) {
        uint32_t value = *d0;
        if (value & 128) { spin(); continue; }
        unsigned n = value & 63;
        if (n > 4 && n <= 7) for (unsigned i = 0; i < n - 4; i++) received(value >> (8 * (i + 1)));
        *d0 = 0x0a4b4f87; /* three bytes: OK\n */
        spin();
    }
}
static void dmseq(void) {
    unsigned s = 0, h = 0, syn = 1;
    for (;;) {
        uint8_t b[4] = {128 | (s << 5) | (h << 4) | (syn << 3) | 2, 'O', '\n', 0};
        b[3] = crc8(b, 3);
        *d1 = 0;
        *d0 = (uint32_t)b[0] | (uint32_t)b[1] << 8 | (uint32_t)b[2] << 16 | (uint32_t)b[3] << 24;
        for (;;) {
            spin();
            uint32_t word = *d0;
            uint8_t a[4] = {word, word >> 8, word >> 16, word >> 24};
            unsigned n = a[0] & 7;
            if ((a[0] & 128) || n > 2 || crc8(a, n + 1) != a[n + 1] || ((a[0] >> 5) & 1) != s) continue;
            unsigned next_h = (a[0] >> 4) & 1;
            if (n && (syn || next_h != h)) for (unsigned i = 0; i < n; i++) received(a[i + 1]);
            if (n) h = next_h;
            break;
        }
        s ^= 1;
        syn = 0;
    }
}
static void rtt(void) {
    const char msg[] = "RTT OK\n";
    unsigned k = 0;
    for (;;) {
        spin();
        struct Ring *up = &_SEGGER_RTT.up, *down = &_SEGGER_RTT.down;
        uint32_t next = (up->wr + 1) & 127;
        if (next != up->rd) {
            up->buffer[up->wr] = msg[k++];
            if (k == sizeof(msg) - 1) k = 0;
            __asm__ volatile ("fence" ::: "memory");
            up->wr = next;
        }
        if (down->rd != down->wr) {
            received(down->buffer[down->rd]);
            down->rd = (down->rd + 1) & 63;
        }
    }
}
int main(void) {
    for (unsigned i = 0; i < 5; i++) result[i] = 0;
    for (unsigned i = 0; i < 32; i++) ram_check[i] = 0x13579bdf ^ i;
    for (unsigned i = 0; i < 32; i++) if (ram_check[i] != (0x13579bdf ^ i)) result[2]++;
    result[0] = 0x534d4f4b; /* SMOK */
    if (CHANNEL == 0) dmdata();
    else if (CHANNEL == 1) dmseq();
    else rtt();
    return 0;
}
