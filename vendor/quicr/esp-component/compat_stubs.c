/*
 * POSIX and wide-character compatibility stubs for ESP-IDF
 * Based on hactar project: https://github.com/Quicr/hactar
 *
 * These stubs provide minimal implementations of POSIX and wide-character
 * functions that libstdc++ and picoquic expect but are not available on ESP-IDF.
 *
 * Wide character support in ESP-IDF newlib is incomplete. libstdc++ requires
 * these functions even if the application doesn't use wide characters directly.
 */

#include <errno.h>
#include <stddef.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <ctype.h>

/* Wide character types - use int for wchar_t operations since ESP-IDF
 * may define wchar_t differently */
typedef unsigned int wint_t_stub;
typedef int wctype_t_stub;
typedef struct { int __count; } mbstate_t_stub;

/*
 * pipe() stub - picoquic uses pipe() for thread wake-up signaling on Unix.
 * ESP-IDF doesn't have pipe(), so we return failure.
 * This disables the wake-up optimization but doesn't break functionality.
 */
int pipe(int pipefd[2])
{
    (void)pipefd;
    errno = ENOSYS;  /* Function not implemented */
    return -1;
}

/*
 * Wide string memory functions
 * These implement narrow character semantics since ESP-IDF doesn't
 * really support wide characters.
 */

void *wmemcpy(void *dest, const void *src, size_t n)
{
    return memcpy(dest, src, n * sizeof(wchar_t));
}

void *wmemmove(void *dest, const void *src, size_t n)
{
    return memmove(dest, src, n * sizeof(wchar_t));
}

void *wmemset(void *s, wchar_t c, size_t n)
{
    wchar_t *p = (wchar_t *)s;
    while (n--) {
        *p++ = c;
    }
    return s;
}

void *wmemchr(const void *s, wchar_t c, size_t n)
{
    const wchar_t *p = (const wchar_t *)s;
    while (n--) {
        if (*p == c) {
            return (void *)p;
        }
        p++;
    }
    return NULL;
}

size_t wcslen(const wchar_t *s)
{
    const wchar_t *p = s;
    while (*p) p++;
    return p - s;
}

/*
 * Wide character case conversion
 * Only handles ASCII range
 */
wint_t_stub towupper(wint_t_stub wc)
{
    if (wc >= 'a' && wc <= 'z') {
        return wc - 'a' + 'A';
    }
    return wc;
}

wint_t_stub towlower(wint_t_stub wc)
{
    if (wc >= 'A' && wc <= 'Z') {
        return wc - 'A' + 'a';
    }
    return wc;
}

/*
 * Wide character classification
 */
wctype_t_stub wctype(const char *property)
{
    (void)property;
    return 0;  /* Return no classification */
}

int iswctype(wint_t_stub wc, wctype_t_stub desc)
{
    (void)wc;
    (void)desc;
    return 0;  /* Not classified */
}

/*
 * Wide/narrow character conversion
 */
int wctob(wint_t_stub c)
{
    if (c < 256) {
        return (int)c;
    }
    return EOF;
}

wint_t_stub btowc(int c)
{
    if (c == EOF) {
        return (wint_t_stub)-1;  /* WEOF */
    }
    return (wint_t_stub)(unsigned char)c;
}

/*
 * Wide character I/O stubs
 * These just return EOF since ESP-IDF doesn't support wide I/O
 */
wint_t_stub getwc(FILE *stream)
{
    (void)stream;
    return (wint_t_stub)-1;  /* WEOF */
}

wint_t_stub fgetwc(FILE *stream)
{
    return getwc(stream);
}

wint_t_stub putwc(wchar_t wc, FILE *stream)
{
    (void)wc;
    (void)stream;
    return (wint_t_stub)-1;  /* WEOF */
}

wint_t_stub fputwc(wchar_t wc, FILE *stream)
{
    return putwc(wc, stream);
}

wint_t_stub ungetwc(wint_t_stub wc, FILE *stream)
{
    (void)wc;
    (void)stream;
    return (wint_t_stub)-1;  /* WEOF */
}

/*
 * Multibyte/wide character conversion
 */
size_t wcrtomb(char *s, wchar_t wc, mbstate_t_stub *ps)
{
    (void)ps;
    if (s == NULL) {
        return 1;
    }
    if (wc < 128) {
        *s = (char)wc;
        return 1;
    }
    errno = EILSEQ;
    return (size_t)-1;
}

/* mbrtowc is provided by newlib - don't stub it */

/*
 * Wide string locale functions
 */
int wcscoll(const wchar_t *ws1, const wchar_t *ws2)
{
    /* Simple comparison without locale */
    while (*ws1 && *ws1 == *ws2) {
        ws1++;
        ws2++;
    }
    return *ws1 - *ws2;
}

size_t wcsxfrm(wchar_t *dest, const wchar_t *src, size_t n)
{
    /* Just copy the string */
    size_t len = wcslen(src);
    if (n > 0) {
        size_t copy = len < n - 1 ? len : n - 1;
        wmemcpy(dest, src, copy);
        dest[copy] = L'\0';
    }
    return len;
}

size_t wcsftime(wchar_t *s, size_t maxsize, const wchar_t *format, const void *timeptr)
{
    (void)format;
    (void)timeptr;
    if (maxsize > 0) {
        s[0] = L'\0';
    }
    return 0;
}

/*
 * String locale transformation
 */
size_t strxfrm(char *dest, const char *src, size_t n)
{
    size_t len = strlen(src);
    if (n > 0) {
        size_t copy = len < n - 1 ? len : n - 1;
        memcpy(dest, src, copy);
        dest[copy] = '\0';
    }
    return len;
}

/*
 * Standard C library functions that may be missing due to link order issues.
 * libstdc++ is linked before libc, so these symbols aren't found.
 * We provide implementations that forward to newlib.
 */

#include <stdarg.h>

/* getc - character input from stream (same as fgetc) */
int getc(FILE *stream)
{
    int c;
    if (stream == NULL) return EOF;
    /* Read one byte */
    if (fread(&c, 1, 1, stream) != 1) return EOF;
    return c & 0xFF;
}

/* strtod - convert string to double */
/* Simple implementation for embedded use */
double strtod(const char *nptr, char **endptr)
{
    double result = 0.0;
    double fraction = 0.1;
    int sign = 1;
    int in_fraction = 0;
    const char *p = nptr;

    /* Skip whitespace */
    while (*p == ' ' || *p == '\t' || *p == '\n') p++;

    /* Handle sign */
    if (*p == '-') { sign = -1; p++; }
    else if (*p == '+') { p++; }

    /* Parse digits */
    while (*p) {
        if (*p >= '0' && *p <= '9') {
            if (in_fraction) {
                result += (*p - '0') * fraction;
                fraction *= 0.1;
            } else {
                result = result * 10.0 + (*p - '0');
            }
        } else if (*p == '.' && !in_fraction) {
            in_fraction = 1;
        } else {
            break;
        }
        p++;
    }

    if (endptr) *endptr = (char *)p;
    return result * sign;
}

/* strtof - convert string to float */
float strtof(const char *nptr, char **endptr)
{
    return (float)strtod(nptr, endptr);
}

/* sscanf - formatted input from string */
/* This is a minimal implementation - full sscanf is complex */
int sscanf(const char *str, const char *format, ...)
{
    (void)str;
    (void)format;
    /* This is a stub - real sscanf would need full format parsing */
    /* libstdc++ uses this for long double conversion, but we return 0 */
    return 0;
}
