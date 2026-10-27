/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * libhost — a shared library that uses what the program loading it
 * exports (a program linked with -rdynamic), as a plug-in uses its host's
 * interface: Python's extension modules do so. */

/* Defined by the program, verification/c/libraries.c. */
int host_value(void);

int host_twice(void)
{
	return 2 * host_value();
}
