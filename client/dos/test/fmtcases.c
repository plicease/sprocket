/* SPDX-License-Identifier: MIT */
/*
 * Formatting cases run on both sides: built for DOS (FMTTEST.EXE, with
 * src/dosfmt.c's printf and sprintf, Open Watcom's 16-bit int and va_list)
 * and for the host (the C library's). test/dosbox-fmt.sh runs FMTTEST.EXE
 * under DOSBox-X and requires its output to be the host's, line for line.
 *
 * Arguments mix 16-bit ints and 32-bit longs, so a formatter that takes
 * the wrong width from the va_list misreads everything after it. Values
 * stay within DOS's int and long, so both sides print the same.
 */
#include <stdio.h>

static char buf[128];

/* What sprintf wrote and returned, as one line. */
static void show(int n)
{
    printf("sprintf %d [%s]\n", n, buf);
}

int main(void)
{
    int n;

    /* Mixed widths, in every order. */
    printf("%d %ld %u %lu\n", -1, -2L, 3u, 4UL);
    printf("%ld %d %lu %u\n", -100000L, 32767, 100000UL, 65535u);
    printf("%u %lu %d %ld %s %c\n", 1u, 70000UL, -2, -70000L, "str", 'z');
    printf("%lx %x %lX %X\n", 0xDEADBEEFUL, 0xBEEFu, 0x12345678UL, 0xABCu);

    /* Boundaries of int, unsigned, long and unsigned long. */
    printf("%d %d %d %d\n", -32767 - 1, 32767, 0, -1);
    printf("%u %u %u\n", 0u, 32768u, 65535u);
    printf("%ld %ld %ld\n", -2147483647L - 1, 2147483647L, 0L);
    printf("%lu %lu %lu\n", 0UL, 2147483648UL, 4294967295UL);
    printf("%x %X %lx %lX\n", 0xFFFFu, 0u, 0xFFFFFFFFUL, 0x80000000UL);

    /* Widths, flags and precisions, with * taking ints between longs. */
    printf("[%*d|%-*ld|%0*lu]\n", 6, -42, 7, -70000L, 12, 4294967295UL);
    printf("[%*s|%-*s|%.*s|%*.*s]\n", 6, "ab", 5, "cd", 2, "efgh", 7, 3, "ijklm");
    printf("[%-*s|%*s]\n", -4, "n", -4, "o");
    printf("[%08lX|%-8lx|%04X|%-6u|%02lu]\n", 0xABCDUL, 0x1FUL, 0xFu, 65535u, 7UL);
    printf("[%5c|%-3c|%%|%s]\n", 'p', 'q', "");

    /* sprintf: the text and the count. */
    n = sprintf(buf, "%ld/%u/%lu/%d", -2147483647L - 1, 65535u, 4294967295UL, -32767 - 1);
    show(n);
    n = sprintf(buf, "%-*.*s|%*lu", 10, 4, "abcdefgh", 11, 123456789UL);
    show(n);
    n = sprintf(buf, "%s", "");
    show(n);

    /* printf's count, after a long argument. */
    n = printf("%lu %d", 4294967295UL, -32767 - 1);
    printf(" -> %d\n", n);

    printf("END\n");
    return 0;
}
