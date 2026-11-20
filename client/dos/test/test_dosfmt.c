/* SPDX-License-Identifier: MIT */
/*
 * The DOS build's printf and sprintf (src/dosfmt.c), on the host: each
 * format SPROCKET uses, and the edge cases of its flags, widths and
 * precisions, against the C library's snprintf. Values stay within DOS's
 * 16-bit int and 32-bit long.
 *
 * dosfmt.c defines printf and sprintf; here they are renamed so that they
 * do not replace the C library's.
 */
#include <stdio.h>
#include <string.h>

#define printf dos_printf
#define sprintf dos_sprintf
#include "../src/dosfmt.c"
#undef printf
#undef sprintf

static int failures;
static int checks;

static void check(const char *what, const char *got, int got_n, const char *want, int want_n)
{
    checks++;
    if (strcmp(got, want) != 0 || got_n != want_n) {
        printf("FAIL %s: got \"%s\" (%d), want \"%s\" (%d)\n", what, got, got_n, want, want_n);
        failures++;
    }
}

/* Format with both, and compare: a macro, so that each call has the
 * arguments' own types. */
#define SAME(...)                                                          \
    do {                                                                   \
        char got[256];                                                     \
        char want[256];                                                    \
        int got_n = dos_sprintf(got, __VA_ARGS__);                         \
        int want_n = snprintf(want, sizeof want, __VA_ARGS__);             \
        check(#__VA_ARGS__, got, got_n, want, want_n);                     \
    } while (0)

int main(void)
{
    /* The formats in SPROCKET's sources. */
    SAME("Session %04X started.", 0x1A2Bu);
    SAME("Session %04X started.", 0x2u);
    SAME("error class %u (code 0x%04x)", 7u, 0xBEEFu);
    SAME("BIOS %02X (%c:) %lu.%02luM", 0x80u, 'A' + 2, 1440UL / 1000, 1440UL % 1000 / 10);
    SAME("BIOS %02X fixed %lu.%luM", 0x81u, 20UL, 3UL);
    SAME("%lu frames, %lu dropped, %u repeated, %u receive errors.", 4000000000UL, 0UL,
         65535u, 0u);
    SAME("Unsupported baud rate %ld.", -1L);
    SAME("Unsupported baud rate %ld.", 115200L);
    SAME("SPROCKET %s, protocol %d.%d, on %s at %ld 8N1.", "0.4.0", 1, 1, "COM1", 9600L);
    SAME("%*s%.*s", 4, "", 3, "abcdef");
    SAME("%.*s\n", 5, "SHORT");
    SAME("%-*.*s  %6s\n", 12, 8, "FILENAME.TXT", "1.4M");
    SAME("%7ld %8lu %lux %luK %ldK %lus %us", -12345L, 99999999UL, 0xFFFFUL, 640UL, -1L,
         59UL, 60u);
    SAME("%c%c%%", 'o', 'k');

    /* Edge cases. */
    SAME("[%d] [%d] [%d]", 0, -32768, 32767);
    SAME("[%i] [%ld] [%ld]", -1, -2147483647L - 1, 2147483647L);
    SAME("[%u] [%lu] [%x] [%lX]", 0u, 4294967295UL, 0xABCDu, 0xDEADBEEFUL);
    SAME("[%5d] [%-5d] [%05d] [%05d] [%-5d]", 42, 42, 42, -42, -42);
    SAME("[%2d] [%02X] [%1s]", 12345, 0xABCu, "long");
    SAME("[%*d] [%-*d] [%*s]", 6, 7, 6, 7, -6, "neg");
    SAME("[%.0s] [%.2s] [%.10s] [%-8.3s]", "abc", "abc", "abc", "abcdef");
    SAME("[%c] [%3c] [%-3c]", 'x', 'y', 'z');
    SAME("%s", "");
    SAME("no conversions at all");
    SAME("%%%d%%", 100);

    /* printf goes to stdout in blocks; its count is the characters. */
    {
        int n;

        fflush(stdout);
        n = dos_printf("%s", "");
        checks++;
        if (n != 0) {
            printf("FAIL dos_printf count: %d\n", n);
            failures++;
        }
    }

    printf("%d checks, %d failures\n", checks, failures);
    return failures != 0;
}
