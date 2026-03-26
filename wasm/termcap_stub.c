/*
 * Minimal termcap stub for libedit in WASM.
 *
 * Since the actual terminal is xterm.js (which handles rendering),
 * we provide hardcoded xterm capabilities so libedit can do cursor
 * movement, line clearing, etc. via ANSI escape sequences.
 */

#include <string.h>
#include <stdio.h>

/* Static buffer for tgetent */
static char termbuf[2048];

/* Hardcoded xterm capabilities */
static struct {
    const char *id;
    const char *value;
} str_caps[] = {
    /* Cursor movement */
    {"cm", "\033[%i%d;%dH"},  /* cursor motion */
    {"up", "\033[A"},          /* cursor up */
    {"do", "\033[B"},          /* cursor down (also \n) */
    {"nd", "\033[C"},          /* cursor right (non-destructive space) */
    {"le", "\b"},              /* cursor left */
    {"cr", "\r"},              /* carriage return */

    /* Line editing */
    {"ce", "\033[K"},          /* clear to end of line */
    {"cd", "\033[J"},          /* clear to end of screen */
    {"cl", "\033[H\033[2J"},   /* clear screen */

    /* Insert/delete */
    {"ic", "\033[@"},          /* insert character */
    {"dc", "\033[P"},          /* delete character */
    {"im", ""},                /* enter insert mode (no-op for xterm) */
    {"ei", ""},                /* exit insert mode */

    /* Keypad */
    {"ks", "\033[?1h\033="},   /* keypad start */
    {"ke", "\033[?1l\033>"},   /* keypad end */

    /* Standout/bold */
    {"so", "\033[7m"},         /* standout begin (reverse) */
    {"se", "\033[27m"},        /* standout end */
    {"us", "\033[4m"},         /* underline begin */
    {"ue", "\033[24m"},        /* underline end */
    {"md", "\033[1m"},         /* bold begin */
    {"me", "\033[0m"},         /* all attributes off */

    /* Bell */
    {"bl", "\007"},            /* bell */

    /* Scroll */
    {"sf", "\n"},              /* scroll forward */
    {"sr", "\033M"},           /* scroll reverse */

    /* Tab */
    {"ta", "\t"},              /* tab */

    {NULL, NULL}
};

static struct {
    const char *id;
    int value;
} num_caps[] = {
    {"co", 80},    /* columns */
    {"li", 24},    /* lines */
    {NULL, 0}
};

static struct {
    const char *id;
    int value;
} bool_caps[] = {
    {"am", 1},     /* auto margins */
    {"km", 1},     /* has meta key */
    {"mi", 1},     /* safe to move in insert mode */
    {"ms", 1},     /* safe to move in standout mode */
    {"xn", 1},     /* newline ignored after 80 cols */
    {NULL, 0}
};

int tgetent(char *bp, const char *name) {
    (void)bp;
    (void)name;
    /* Always succeed — we support any terminal name */
    return 1;
}

char *tgetstr(const char *id, char **area) {
    for (int i = 0; str_caps[i].id; i++) {
        if (strcmp(id, str_caps[i].id) == 0) {
            if (area && *area) {
                char *p = *area;
                strcpy(p, str_caps[i].value);
                *area += strlen(str_caps[i].value) + 1;
                return p;
            }
            return (char *)str_caps[i].value;
        }
    }
    return NULL;
}

int tgetnum(const char *id) {
    for (int i = 0; num_caps[i].id; i++) {
        if (strcmp(id, num_caps[i].id) == 0)
            return num_caps[i].value;
    }
    return -1;
}

int tgetflag(const char *id) {
    for (int i = 0; bool_caps[i].id; i++) {
        if (strcmp(id, bool_caps[i].id) == 0)
            return bool_caps[i].value;
    }
    return 0;
}

/* tputs: output a string with padding (we ignore padding delays) */
static int (*tputs_putc_fn)(int);

int tputs(const char *str, int affcnt, int (*putc_fn)(int)) {
    (void)affcnt;
    if (!str) return 0;
    while (*str) {
        putc_fn((unsigned char)*str++);
    }
    return 0;
}

/* tgoto: simple cursor addressing — decode %d/%i format */
static char tgoto_buf[64];

char *tgoto(const char *cap, int col, int row) {
    if (!cap) return NULL;
    snprintf(tgoto_buf, sizeof(tgoto_buf), "\033[%d;%dH", row + 1, col + 1);
    return tgoto_buf;
}

/* tparm: parameterized string (simplified — handles up to 2 int params) */
char *tparm(const char *str, ...) {
    /* For our purposes tgoto handles the main case */
    return (char *)str;
}
