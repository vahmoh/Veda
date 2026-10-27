/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * libbase — a shared library that another needs (libplugin, which names
 * it in DT_NEEDED and finds it beside itself through $ORIGIN), for
 * verification/c/libraries.c: a function, data, and a constructor that
 * runs before its dependants'. */

int base_constructed;
int base_counter;

static void __attribute__((constructor)) construct(void)
{
	base_constructed = 1;
}

int base_value(void)
{
	return 40 + ++base_counter;
}
