/*
 * Minimal termcap/curses headers for libedit in WASM.
 * Provides function declarations matching termcap_stub.c.
 */

#ifndef TERMCAP_STUB_H
#define TERMCAP_STUB_H

int   tgetent(char *bp, const char *name);
char *tgetstr(const char *id, char **area);
int   tgetnum(const char *id);
int   tgetflag(const char *id);
int   tputs(const char *str, int affcnt, int (*putc_fn)(int));
char *tgoto(const char *cap, int col, int row);

#endif
