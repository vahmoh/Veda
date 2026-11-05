/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * checks — checks of curl and libcurl on Veda.
 *
 * Each check prints "ok - NAME" or "not ok - NAME: REASON"; the program
 * exits with the number of failed checks (0: all passed). `systest` runs it
 * inside Veda in a scratch directory, with its standard output on a pipe:
 * `checks`, or `checks host ADDRESS TCP-PORT UDP-PORT` to reach the host
 * machine's web server too (see check_host).
 *
 * Servers of its own, threads on the loopback interface, stand in for the
 * web. One port takes each connection as it begins: TLS (1.2 or 1.3, its
 * certificate a test authority's, HTTP/2 or HTTP/1.1 by ALPN), HTTP/2 by
 * prior knowledge, or HTTP/1.1, as a proxy too (absolute targets, and
 * CONNECT's tunnels, which begin anew); another port asks for the client's
 * certificate; a Unix domain socket takes HTTP/1.1. libcurl is checked in
 * this process, and the curl program (/system/bin/curl) in processes it
 * starts. The versions expected of curl and of the libraries libcurl holds
 * are their ports' (VEDA_*_VERSION, which the build defines). */

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <brotli/encode.h>
#include <ctype.h>
#include <curl/curl.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <locale.h>
#include <netinet/in.h>
#include <nghttp2/nghttp2.h>
#include <openssl/evp.h>
#include <openssl/pem.h>
#include <openssl/ssl.h>
#include <openssl/x509.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <zlib.h>
#include <zstd.h>

#if !defined(VEDA_CURL_VERSION) || !defined(VEDA_OPENSSL_VERSION) || !defined(VEDA_NGHTTP2_VERSION) ||  \
	!defined(VEDA_ZLIB_VERSION) || !defined(VEDA_BROTLI_VERSION) || !defined(VEDA_ZSTD_VERSION) || \
	!defined(VEDA_LIBIDN2_VERSION) || !defined(VEDA_LIBPSL_VERSION)
#error "the build defines the versions of curl's ports (VEDA_CURL_VERSION and the others)"
#endif

extern char **environ;

static int failures;

static void report(const char *name, int ok, const char *fmt, ...)
{
	if (ok) {
		printf("ok - %s\n", name);
		return;
	}
	failures++;
	printf("not ok - %s: ", name);
	va_list ap;
	va_start(ap, fmt);
	vprintf(fmt, ap);
	va_end(ap);
	printf("\n");
}

/* The condition first: the message may show what it computed. */
#define CHECK(name, cond, ...)                  \
	do {                                    \
		int ok_ = (cond);               \
		report(name, ok_, __VA_ARGS__); \
	} while (0)

/* A test authority, which signed the servers' certificate (localhost,
 * 127.0.0.1 and ::1, valid from 2026 to 2126), an expired one
 * (expired.example, 2020) and a client's ("Veda test client"), all P-256:
 * made for these checks, and trusted by nothing else. */
static const char CA_CERT[] =
	"-----BEGIN CERTIFICATE-----\n"
	"MIIBojCCAUmgAwIBAgIUH+INHuywR7GKipe8iqkDhnnqBPUwCgYIKoZIzj0EAwIw\n"
	"HjEcMBoGA1UEAwwTVmVkYSB0ZXN0IGF1dGhvcml0eTAgFw0yNjAxMDEwMDAwMDBa\n"
	"GA8yMTI2MDEwMTAwMDAwMFowHjEcMBoGA1UEAwwTVmVkYSB0ZXN0IGF1dGhvcml0\n"
	"eTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABHRkCatSZBtXz8zWKxlc9LT9b0hZ\n"
	"LtHtf8FwDheTTzCyiUlyaz30skS/CkIyN41dhLz77FOec9HUwheDpSs4VO6jYzBh\n"
	"MB0GA1UdDgQWBBRNmkaPq1HneHJMYh3ftkKaE4HxiTAfBgNVHSMEGDAWgBRNmkaP\n"
	"q1HneHJMYh3ftkKaE4HxiTAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwIB\n"
	"BjAKBggqhkjOPQQDAgNHADBEAiBjQ1BcADQe28pyHietE/Lbs/rtoo/jveBQBm6W\n"
	"z7Rr+gIgR8/lxt6d3kJcQx+hOsLZigY8Lju+gaPRFXB5+Tw2AmY=\n"
	"-----END CERTIFICATE-----\n";
static const char SERVER_CERT[] =
	"-----BEGIN CERTIFICATE-----\n"
	"MIIByDCCAW6gAwIBAgIBAjAKBggqhkjOPQQDAjAeMRwwGgYDVQQDDBNWZWRhIHRl\n"
	"c3QgYXV0aG9yaXR5MCAXDTI2MDEwMTAwMDAwMFoYDzIxMjYwMTAxMDAwMDAwWjAU\n"
	"MRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAATR\n"
	"sseeGKXXjH98FyWYEgbpiqgOADN3viNnznanoyFh6tjO+S8n6eQmwJynOvHQxk7T\n"
	"haLlSfu/jVy59ByVJFEoo4GkMIGhMAwGA1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQD\n"
	"AgeAMBMGA1UdJQQMMAoGCCsGAQUFBwMBMCwGA1UdEQQlMCOCCWxvY2FsaG9zdIcE\n"
	"fwAAAYcQAAAAAAAAAAAAAAAAAAAAATAdBgNVHQ4EFgQUtOz1DkCsGds4C2M893TA\n"
	"lIuwXJMwHwYDVR0jBBgwFoAUTZpGj6tR53hyTGId37ZCmhOB8YkwCgYIKoZIzj0E\n"
	"AwIDSAAwRQIhALF1CwFXFbNIah6+5pqjx46x7tDva7hIjGXsjrLooUTsAiBx/xJ0\n"
	"AMBX6ge5s0ZRzfBG9efDARI/6qd7ZQGP3EXD4Q==\n"
	"-----END CERTIFICATE-----\n";
static const char SERVER_KEY[] =
	"-----BEGIN PRIVATE KEY-----\n"
	"MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQglwBVldnfICK0MyK0\n"
	"xLqpAhNdGxnhy+n4K0hif51u1aehRANCAATRsseeGKXXjH98FyWYEgbpiqgOADN3\n"
	"viNnznanoyFh6tjO+S8n6eQmwJynOvHQxk7ThaLlSfu/jVy59ByVJFEo\n"
	"-----END PRIVATE KEY-----\n";
static const char EXPIRED_CERT[] =
	"-----BEGIN CERTIFICATE-----\n"
	"MIIBuTCCAWCgAwIBAgIBBDAKBggqhkjOPQQDAjAeMRwwGgYDVQQDDBNWZWRhIHRl\n"
	"c3QgYXV0aG9yaXR5MB4XDTIwMDEwMTAwMDAwMFoXDTIxMDEwMTAwMDAwMFowGjEY\n"
	"MBYGA1UEAwwPZXhwaXJlZC5leGFtcGxlMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcD\n"
	"QgAE0bLHnhil14x/fBclmBIG6YqoDgAzd74jZ852p6MhYerYzvkvJ+nkJsCcpzrx\n"
	"0MZO04Wi5Un7v41cufQclSRRKKOBkjCBjzAMBgNVHRMBAf8EAjAAMA4GA1UdDwEB\n"
	"/wQEAwIHgDATBgNVHSUEDDAKBggrBgEFBQcDATAaBgNVHREEEzARgg9leHBpcmVk\n"
	"LmV4YW1wbGUwHQYDVR0OBBYEFLTs9Q5ArBnbOAtjPPd0wJSLsFyTMB8GA1UdIwQY\n"
	"MBaAFE2aRo+rUed4ckxiHd+2QpoTgfGJMAoGCCqGSM49BAMCA0cAMEQCIBi5yAUY\n"
	"cQJ+AmenBHTtsH9HFPoWYGD+5EzAcD0OAq7/AiAsmtmMddqmDitU7deYs2IIsnem\n"
	"dfl/CLJbl9ZGr9pupg==\n"
	"-----END CERTIFICATE-----\n";
static const char CLIENT_CERT[] =
	"-----BEGIN CERTIFICATE-----\n"
	"MIIBnzCCAUWgAwIBAgIBAzAKBggqhkjOPQQDAjAeMRwwGgYDVQQDDBNWZWRhIHRl\n"
	"c3QgYXV0aG9yaXR5MCAXDTI2MDEwMTAwMDAwMFoYDzIxMjYwMTAxMDAwMDAwWjAb\n"
	"MRkwFwYDVQQDDBBWZWRhIHRlc3QgY2xpZW50MFkwEwYHKoZIzj0CAQYIKoZIzj0D\n"
	"AQcDQgAEE/cMJfawQ/+F/32C3kioh5xJA+1ZnPHzxnlyCDxXqucZrpScAb64bisd\n"
	"v75NtKlylEy5cgRvXNRJ58vqrdLID6N1MHMwDAYDVR0TAQH/BAIwADAOBgNVHQ8B\n"
	"Af8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwIwHQYDVR0OBBYEFE+Rq6SiBXdT\n"
	"tIk3wvo2plfNmpHeMB8GA1UdIwQYMBaAFE2aRo+rUed4ckxiHd+2QpoTgfGJMAoG\n"
	"CCqGSM49BAMCA0gAMEUCIGylPQsu3HcTFOBqcsZIgdnJqXFfNfGxfwoBdCJhVOYt\n"
	"AiEAq+ByFzMr0lwrE2a3/Gywc9US6JHr2bTSP9zlxyPO5wc=\n"
	"-----END CERTIFICATE-----\n";
static const char CLIENT_KEY[] =
	"-----BEGIN PRIVATE KEY-----\n"
	"MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWD0tsUHHegIhjYHQ\n"
	"yHAVd+goOqoyYriMvITH1VNaF+yhRANCAAQT9wwl9rBD/4X/fYLeSKiHnEkD7Vmc\n"
	"8fPGeXIIPFeq5xmulJwBvrhuKx2/vk20qXKUTLlyBG9c1Enny+qt0sgP\n"
	"-----END PRIVATE KEY-----\n";

/* What the compressed responses hold, once decompressed. */
static const char TEXT[] = "curl on Veda takes gzip, deflate, Brotli and Zstandard, and gives back this line.\n";

/* The most a request's body may hold. */
#define MAX_BODY (64u << 20)

static double now(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

/* The byte at OFFSET of /big's content, the same in every response, so
 * that any range of it can be checked. */
static unsigned char big_byte(uint64_t offset)
{
	return (unsigned char)((offset * 0x9E3779B97F4A7C15ull) >> 56);
}

/* The CRC-32 of /big's bytes FROM to TO (not included). */
static unsigned long big_crc(uint64_t from, uint64_t to)
{
	unsigned char chunk[4096];
	unsigned long crc = crc32(0, 0, 0);
	while (from < to) {
		size_t n = to - from < sizeof chunk ? (size_t)(to - from) : sizeof chunk;
		for (size_t i = 0; i < n; i++)
			chunk[i] = big_byte(from + i);
		crc = crc32(crc, chunk, (uInt)n);
		from += n;
	}
	return crc;
}

static unsigned long crc_of(const void *data, size_t len)
{
	return crc32(crc32(0, 0, 0), data, (uInt)len);
}

struct buffer {
	char *data;
	size_t len;
};

static void empty(struct buffer *b)
{
	free(b->data);
	b->data = 0;
	b->len = 0;
}

static int write_file(const char *path, const void *data, size_t len)
{
	FILE *f = fopen(path, "wb");
	if (!f)
		return -1;
	int ok = fwrite(data, 1, len, f) == len;
	return fclose(f) == 0 && ok ? 0 : -1;
}

/* The file at PATH, NUL-terminated; no data if it cannot be read. */
static struct buffer read_file(const char *path)
{
	struct buffer b = { 0 };
	FILE *f = fopen(path, "rb");
	if (!f)
		return b;
	char chunk[65536];
	size_t n;
	while ((n = fread(chunk, 1, sizeof chunk, f)) > 0) {
		char *more = realloc(b.data, b.len + n + 1);
		if (!more)
			break;
		b.data = more;
		memcpy(b.data + b.len, chunk, n);
		b.len += n;
	}
	fclose(f);
	if (!b.data)
		b.data = calloc(1, 1);
	else
		b.data[b.len] = 0;
	return b;
}

/* --- the servers ------------------------------------------------------------ */

/* What the servers saw, for the checks to ask. */
static atomic_int h2_connections;
static atomic_int tunnels;
static atomic_int stalled;

/* The main port, on 127.0.0.1 and ::1; the one that asks for the client's
 * certificate; the Unix domain socket's path. */
static int port, mutual_port;
static char unix_path[sizeof ((struct sockaddr_un *)0)->sun_path];

static X509 *expired_cert;
static EVP_PKEY *server_key;
static pthread_attr_t detached;

/* The compressed forms of TEXT. */
struct blob {
	unsigned char *data;
	size_t len;
};
static struct blob gzipped, deflated, brotli_text, zstd_text;

/* A connection to a server: its socket, its TLS once that has begun, and
 * what was read of it but not yet taken. */
struct conn {
	int fd;
	SSL *ssl;
	SSL_CTX *tls; /* the TLS a connection may begin with, if any */
	size_t have;
	unsigned char buf[65536];
};

static ssize_t conn_read(struct conn *c, void *p, size_t n)
{
	if (c->ssl) {
		int r = SSL_read(c->ssl, p, n > INT_MAX ? INT_MAX : (int)n);
		return r > 0 ? r : SSL_get_error(c->ssl, r) == SSL_ERROR_ZERO_RETURN ? 0 : -1;
	}
	ssize_t r;
	do
		r = recv(c->fd, p, n, 0);
	while (r < 0 && errno == EINTR);
	return r;
}

static int conn_write(struct conn *c, const void *p, size_t n)
{
	const unsigned char *b = p;
	while (n > 0) {
		ssize_t w;
		if (c->ssl) {
			int r = SSL_write(c->ssl, b, n > INT_MAX ? INT_MAX : (int)n);
			w = r > 0 ? r : -1;
		} else {
			do
				w = send(c->fd, b, n, MSG_NOSIGNAL);
			while (w < 0 && errno == EINTR);
		}
		if (w <= 0)
			return -1;
		b += w;
		n -= (size_t)w;
	}
	return 0;
}

static int say(struct conn *c, const char *s)
{
	return conn_write(c, s, strlen(s));
}

/* Reads more into the connection's buffer: what came, 0 at its end. */
static ssize_t fill(struct conn *c)
{
	if (c->have == sizeof c->buf)
		return -1;
	ssize_t n = conn_read(c, c->buf + c->have, sizeof c->buf - c->have);
	if (n > 0)
		c->have += (size_t)n;
	return n;
}

static void take(struct conn *c, size_t n)
{
	memmove(c->buf, c->buf + n, c->have - n);
	c->have -= n;
}

static int read_exactly(struct conn *c, unsigned char *to, size_t n)
{
	while (n > 0) {
		if (c->have == 0 && fill(c) <= 0)
			return -1;
		size_t k = c->have < n ? c->have : n;
		memcpy(to, c->buf, k);
		take(c, k);
		to += k;
		n -= k;
	}
	return 0;
}

/* A line, without its CRLF. */
static int read_line(struct conn *c, char *line, size_t size)
{
	for (;;) {
		unsigned char *end = memmem(c->buf, c->have, "\r\n", 2);
		if (end) {
			size_t n = (size_t)(end - c->buf);
			if (n >= size)
				return -1;
			memcpy(line, c->buf, n);
			line[n] = 0;
			take(c, n + 2);
			return 0;
		}
		if (fill(c) <= 0)
			return -1;
	}
}

struct request {
	char method[16];
	char target[2048];
	const char *path; /* the target's path and query, past a proxy's authority */
	char head[16384]; /* as received; HTTP/2's as "name: value" lines */
	size_t head_len;
	unsigned char *body;
	size_t body_len;
	struct conn *conn;
	int h2;
};

/* The value of the request's header NAME (its first), into VALUE. */
static int header(const struct request *q, const char *name, char *value, size_t size)
{
	size_t len = strlen(name);
	for (const char *line = q->head; line && *line; line = strchr(line, '\n'), line = line ? line + 1 : 0) {
		if (strncasecmp(line, name, len) || line[len] != ':')
			continue;
		const char *v = line + len + 1;
		while (*v == ' ' || *v == '\t')
			v++;
		size_t n = strcspn(v, "\r\n");
		if (n >= size)
			n = size - 1;
		memcpy(value, v, n);
		value[n] = 0;
		return 1;
	}
	return 0;
}

struct response {
	int status;
	char headers[4096]; /* "Name: value" lines, each with its CRLF */
	unsigned char *body;
	size_t len;
	int chunked; /* sent in chunks (HTTP/1.1) */
	int stall;   /* seconds to wait before the body */
};

static void add_header(struct response *r, const char *fmt, ...)
{
	size_t used = strlen(r->headers);
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(r->headers + used, sizeof r->headers - used, fmt, ap);
	va_end(ap);
	used = strlen(r->headers);
	snprintf(r->headers + used, sizeof r->headers - used, "\r\n");
}

static void set_body(struct response *r, const void *data, size_t len)
{
	r->body = malloc(len ? len : 1);
	if (len)
		memcpy(r->body, data, len);
	r->len = len;
}

static void text(struct response *r, const char *fmt, ...)
{
	char buf[1024];
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(buf, sizeof buf, fmt, ap);
	va_end(ap);
	add_header(r, "Content-Type: text/plain");
	set_body(r, buf, strlen(buf));
}

/* /big?n=N: N bytes, or the range of them asked for. */
static void big(const struct request *q, struct response *r, uint64_t n)
{
	char range[128];
	unsigned long long a, b;
	uint64_t from = 0, to = n;
	if (n > MAX_BODY) {
		r->status = 400;
		text(r, "too big\n");
		return;
	}
	if (header(q, "Range", range, sizeof range)) {
		if (sscanf(range, "bytes=%llu-%llu", &a, &b) == 2 && a <= b && b < n) {
			from = a;
			to = b + 1;
		} else if (sscanf(range, "bytes=%llu-", &a) == 1 && a < n) {
			from = a;
		} else {
			r->status = 416;
			add_header(r, "Content-Range: bytes */%llu", (unsigned long long)n);
			text(r, "no such range\n");
			return;
		}
		r->status = 206;
		add_header(r, "Content-Range: bytes %llu-%llu/%llu", (unsigned long long)from,
			   (unsigned long long)to - 1, (unsigned long long)n);
	}
	add_header(r, "Accept-Ranges: bytes");
	r->len = to - from;
	r->body = malloc(r->len ? r->len : 1);
	for (uint64_t i = from; i < to; i++)
		r->body[i - from] = big_byte(i);
}

/* What /tls says of the connection: its TLS version, the protocol ALPN
 * chose and the client's certificate. */
static void describe_tls(const struct request *q, struct response *r)
{
	SSL *ssl = q->conn->ssl;
	if (!ssl) {
		text(r, "plain %s\n", q->h2 ? "h2c" : "http/1.1");
		return;
	}
	const unsigned char *alpn = 0;
	unsigned int alpn_len = 0;
	SSL_get0_alpn_selected(ssl, &alpn, &alpn_len);
	char client[256] = "none";
	X509 *peer = SSL_get0_peer_certificate(ssl);
	if (peer)
		X509_NAME_get_text_by_NID(X509_get_subject_name(peer), NID_commonName, client, sizeof client);
	text(r, "%s %.*s client=%s\n", SSL_get_version(ssl), alpn_len ? (int)alpn_len : 1,
	     alpn_len ? (const char *)alpn : "-", client);
}

static void route(struct request *q, struct response *r)
{
	const char *p = q->path;
	char value[1024];
	const struct blob *encoded = 0;
	r->status = 200;
	if (!strcmp(p, "/hello")) {
		text(r, "hello from Veda\n");
	} else if (!strcmp(p, "/redirect")) {
		r->status = 302;
		add_header(r, "Location: /hello");
		text(r, "moved\n");
	} else if (!strcmp(p, "/redirect-loop")) {
		r->status = 302;
		add_header(r, "Location: /redirect-loop");
		text(r, "again\n");
	} else if (!strncmp(p, "/status/", 8)) {
		r->status = atoi(p + 8);
		text(r, "status %d\n", r->status);
	} else if (!strcmp(p, "/echo")) {
		add_header(r, "X-Method: %s", q->method);
		set_body(r, q->body, q->body_len);
	} else if (!strcmp(p, "/headers")) {
		set_body(r, q->head, q->head_len);
	} else if (!strcmp(p, "/cookies/set")) {
		/* The second is for a public suffix, which no site may set. */
		add_header(r, "Set-Cookie: plain=1; Path=/");
		add_header(r, "Set-Cookie: suffix=1; Domain=co.uk; Path=/");
		add_header(r, "Set-Cookie: site=1; Domain=example.co.uk; Path=/");
		text(r, "cookies set\n");
	} else if (!strcmp(p, "/cookies")) {
		if (!header(q, "Cookie", value, sizeof value))
			strcpy(value, "none");
		text(r, "%s\n", value);
	} else if (!strcmp(p, "/gzip") || !strcmp(p, "/deflate") || !strcmp(p, "/br") || !strcmp(p, "/zstd")) {
		encoded = p[1] == 'g' ? &gzipped : p[1] == 'd' ? &deflated : p[1] == 'b' ? &brotli_text : &zstd_text;
		add_header(r, "Content-Encoding: %s", p + 1);
		set_body(r, encoded->data, encoded->len);
	} else if (!strcmp(p, "/chunked")) {
		r->chunked = !q->h2;
		text(r, "%s", TEXT);
	} else if (!strncmp(p, "/big?n=", 7)) {
		big(q, r, strtoull(p + 7, 0, 10));
	} else if (!strcmp(p, "/slow")) {
		r->stall = 30;
		atomic_fetch_add(&stalled, 1);
		text(r, "at last\n");
	} else if (!strcmp(p, "/auth")) {
		/* veda:secret */
		if (header(q, "Authorization", value, sizeof value) && !strcmp(value, "Basic dmVkYTpzZWNyZXQ=")) {
			text(r, "welcome\n");
		} else {
			r->status = 401;
			add_header(r, "WWW-Authenticate: Basic realm=\"veda\"");
			text(r, "who are you?\n");
		}
	} else if (!strcmp(p, "/tls")) {
		describe_tls(q, r);
	} else if (!strcmp(p, "/hsts")) {
		if (q->conn->ssl)
			add_header(r, "Strict-Transport-Security: max-age=86400");
		text(r, "remember me\n");
	} else if (!strcmp(p, "/time")) {
		add_header(r, "Last-Modified: Wed, 21 Oct 2015 07:28:00 GMT");
		text(r, "timed\n");
	} else {
		r->status = 404;
		text(r, "nothing here\n");
	}
}

static const char *reason(int status)
{
	switch (status) {
	case 200:
		return "OK";
	case 206:
		return "Partial Content";
	case 302:
		return "Found";
	case 401:
		return "Unauthorized";
	case 404:
		return "Not Found";
	case 416:
		return "Range Not Satisfiable";
	default:
		return "Whatever";
	}
}

/* Waits SECONDS before a response's body, or until the client does
 * something (sends, or goes away), which one waiting for it does not. */
static int stall(struct conn *c, int seconds)
{
	struct pollfd p = { .fd = c->fd, .events = POLLIN };
	return poll(&p, 1, seconds * 1000) == 0 ? 0 : -1;
}

static int send_response(struct conn *c, const struct request *q, const struct response *r, int close)
{
	char head[8192];
	int n;
	if (r->chunked)
		n = snprintf(head, sizeof head, "HTTP/1.1 %d %s\r\n%sTransfer-Encoding: chunked\r\n%s\r\n", r->status,
			     reason(r->status), r->headers, close ? "Connection: close\r\n" : "");
	else
		n = snprintf(head, sizeof head, "HTTP/1.1 %d %s\r\n%sContent-Length: %zu\r\n%s\r\n", r->status,
			     reason(r->status), r->headers, r->len, close ? "Connection: close\r\n" : "");
	if (n < 0 || (size_t)n >= sizeof head || conn_write(c, head, (size_t)n))
		return -1;
	if (!strcmp(q->method, "HEAD"))
		return 0;
	if (r->stall && stall(c, r->stall))
		return -1;
	if (!r->chunked)
		return conn_write(c, r->body, r->len);
	/* In chunks of seven bytes, then the last, empty one. */
	for (size_t at = 0; at < r->len; at += 7) {
		size_t k = r->len - at < 7 ? r->len - at : 7;
		char size[32];
		int m = snprintf(size, sizeof size, "%zx\r\n", k);
		if (conn_write(c, size, (size_t)m) || conn_write(c, r->body + at, k) || say(c, "\r\n"))
			return -1;
	}
	return say(c, "0\r\n\r\n");
}

static int read_chunked(struct conn *c, struct request *q)
{
	char line[256];
	for (;;) {
		if (read_line(c, line, sizeof line))
			return -1;
		size_t n = strtoul(line, 0, 16);
		if (n == 0)
			break;
		if (q->body_len + n > MAX_BODY)
			return -1;
		unsigned char *more = realloc(q->body, q->body_len + n);
		if (!more)
			return -1;
		q->body = more;
		if (read_exactly(c, q->body + q->body_len, n) || read_line(c, line, sizeof line))
			return -1;
		q->body_len += n;
	}
	/* The trailers, to the empty line. */
	do
		if (read_line(c, line, sizeof line))
			return -1;
	while (*line);
	return 0;
}

/* A WebSocket (RFC 6455): every message comes back, a ping as a pong, and
 * a close is answered with one. */
static void serve_websocket(struct conn *c, const struct request *q)
{
	char key[128], both[256], accept[64], head[256];
	unsigned char digest[EVP_MAX_MD_SIZE];
	unsigned int len;
	if (!header(q, "Sec-WebSocket-Key", key, sizeof key))
		return;
	snprintf(both, sizeof both, "%s258EAFA5-E914-47DA-95CA-C5AB0DC85B11", key);
	if (!EVP_Digest(both, strlen(both), digest, &len, EVP_sha1(), 0))
		return;
	EVP_EncodeBlock((unsigned char *)accept, digest, (int)len);
	snprintf(head, sizeof head,
		 "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
		 "Sec-WebSocket-Accept: %s\r\n\r\n",
		 accept);
	if (say(c, head))
		return;
	for (;;) {
		unsigned char h[10], mask[4] = { 0 }, out[10];
		if (read_exactly(c, h, 2))
			return;
		int opcode = h[0] & 0x0f;
		uint64_t n = h[1] & 0x7f;
		if (n == 126) {
			if (read_exactly(c, h + 2, 2))
				return;
			n = (uint64_t)h[2] << 8 | h[3];
		} else if (n == 127) {
			if (read_exactly(c, h + 2, 8))
				return;
			n = 0;
			for (int i = 0; i < 8; i++)
				n = n << 8 | h[2 + i];
		}
		if (((h[1] & 0x80) && read_exactly(c, mask, 4)) || n > (1u << 20))
			return;
		unsigned char *payload = malloc(n ? n : 1);
		if (!payload || read_exactly(c, payload, n)) {
			free(payload);
			return;
		}
		for (uint64_t i = 0; i < n; i++)
			payload[i] ^= mask[i % 4];
		/* The same frame back, unmasked, as a server's are. */
		size_t m = 0;
		out[m++] = (unsigned char)((h[0] & 0x80) | (opcode == 0x9 ? 0xA : opcode));
		if (n < 126) {
			out[m++] = (unsigned char)n;
		} else if (n < 65536) {
			out[m++] = 126;
			out[m++] = (unsigned char)(n >> 8);
			out[m++] = (unsigned char)n;
		} else {
			out[m++] = 127;
			for (int i = 7; i >= 0; i--)
				out[m++] = (unsigned char)(n >> (8 * i));
		}
		int failed = conn_write(c, out, m) || conn_write(c, payload, n);
		free(payload);
		if (failed || opcode == 0x8)
			return;
	}
}

static void serve(struct conn *c);
static void serve_h2(struct conn *c, const unsigned char *read, size_t len);

static void serve_http1(struct conn *c)
{
	struct request *q = calloc(1, sizeof *q);
	char value[256], version[16];
	while (q) {
		/* The head. */
		unsigned char *end;
		while (!(end = memmem(c->buf, c->have, "\r\n\r\n", 4)))
			if (fill(c) <= 0)
				goto done;
		size_t len = (size_t)(end - c->buf) + 4;
		if (len >= sizeof q->head)
			goto done;
		memset(q, 0, sizeof *q);
		memcpy(q->head, c->buf, len);
		q->head_len = len;
		q->conn = c;
		take(c, len);
		if (sscanf(q->head, "%15s %2047s %15s", q->method, q->target, version) != 3)
			goto done;
		/* HTTP/2 by prior knowledge: its preface is a request of its own. */
		if (!strcmp(q->method, "PRI") && !strcmp(version, "HTTP/2.0")) {
			serve_h2(c, (const unsigned char *)q->head, len);
			goto done;
		}
		/* What a proxy is asked for names the server: its path is what
		   is served. */
		q->path = q->target;
		if (!strncmp(q->target, "http://", 7) || !strncmp(q->target, "https://", 8)) {
			const char *slash = strchr(strstr(q->target, "//") + 2, '/');
			q->path = slash ? slash : "/";
		}
		/* A tunnel, through which a connection begins anew. */
		if (!strcmp(q->method, "CONNECT")) {
			atomic_fetch_add(&tunnels, 1);
			if (!say(c, "HTTP/1.1 200 Connection established\r\n\r\n"))
				serve(c);
			goto done;
		}
		if (header(q, "Upgrade", value, sizeof value) && !strcasecmp(value, "websocket") &&
		    !strcmp(q->path, "/ws")) {
			serve_websocket(c, q);
			goto done;
		}
		/* The body: its length, or chunks. */
		if (header(q, "Expect", value, sizeof value) && !strcasecmp(value, "100-continue") &&
		    say(c, "HTTP/1.1 100 Continue\r\n\r\n"))
			goto done;
		if (header(q, "Transfer-Encoding", value, sizeof value) && strstr(value, "chunked")) {
			if (read_chunked(c, q))
				goto done;
		} else if (header(q, "Content-Length", value, sizeof value)) {
			q->body_len = strtoull(value, 0, 10);
			if (q->body_len > MAX_BODY || !(q->body = malloc(q->body_len ? q->body_len : 1)) ||
			    read_exactly(c, q->body, q->body_len))
				goto done;
		}
		struct response r = { 0 };
		route(q, &r);
		int close = header(q, "Connection", value, sizeof value) && !strcasecmp(value, "close");
		int failed = send_response(c, q, &r, close);
		free(r.body);
		free(q->body);
		q->body = 0;
		if (failed || close)
			goto done;
	}
done:
	if (q)
		free(q->body);
	free(q);
}

/* HTTP/2 (RFC 9113), by nghttp2's server: a stream for each request. */
struct h2_stream {
	struct request q;
	struct response r;
	size_t sent;
};

static nghttp2_ssize h2_send(nghttp2_session *s, const uint8_t *data, size_t len, int flags, void *user)
{
	(void)s;
	(void)flags;
	return conn_write(user, data, len) ? NGHTTP2_ERR_CALLBACK_FAILURE : (nghttp2_ssize)len;
}

static int h2_begin_headers(nghttp2_session *s, const nghttp2_frame *f, void *user)
{
	if (f->hd.type != NGHTTP2_HEADERS || f->headers.cat != NGHTTP2_HCAT_REQUEST)
		return 0;
	struct h2_stream *st = calloc(1, sizeof *st);
	if (!st)
		return NGHTTP2_ERR_CALLBACK_FAILURE;
	st->q.conn = user;
	st->q.h2 = 1;
	return nghttp2_session_set_stream_user_data(s, f->hd.stream_id, st);
}

static int h2_header(nghttp2_session *s, const nghttp2_frame *f, const uint8_t *name, size_t namelen,
		     const uint8_t *value, size_t valuelen, uint8_t flags, void *user)
{
	(void)flags;
	(void)user;
	struct h2_stream *st = nghttp2_session_get_stream_user_data(s, f->hd.stream_id);
	if (!st)
		return 0;
	struct request *q = &st->q;
	size_t room = sizeof q->head - q->head_len;
	int n = snprintf(q->head + q->head_len, room, "%.*s: %.*s\r\n", (int)namelen, (const char *)name,
			 (int)valuelen, (const char *)value);
	if (n > 0 && (size_t)n < room)
		q->head_len += (size_t)n;
	if (namelen == 7 && !memcmp(name, ":method", 7))
		snprintf(q->method, sizeof q->method, "%.*s", (int)valuelen, (const char *)value);
	else if (namelen == 5 && !memcmp(name, ":path", 5))
		snprintf(q->target, sizeof q->target, "%.*s", (int)valuelen, (const char *)value);
	return 0;
}

static int h2_data(nghttp2_session *s, uint8_t flags, int32_t id, const uint8_t *data, size_t len, void *user)
{
	(void)flags;
	(void)user;
	struct h2_stream *st = nghttp2_session_get_stream_user_data(s, id);
	if (!st)
		return 0;
	unsigned char *more = st->q.body_len + len <= MAX_BODY ? realloc(st->q.body, st->q.body_len + len) : 0;
	if (!more)
		return NGHTTP2_ERR_CALLBACK_FAILURE;
	st->q.body = more;
	memcpy(st->q.body + st->q.body_len, data, len);
	st->q.body_len += len;
	return 0;
}

static nghttp2_ssize h2_body(nghttp2_session *s, int32_t id, uint8_t *buf, size_t len, uint32_t *flags,
			     nghttp2_data_source *source, void *user)
{
	(void)s;
	(void)id;
	(void)user;
	struct h2_stream *st = source->ptr;
	size_t n = st->r.len - st->sent < len ? st->r.len - st->sent : len;
	memcpy(buf, st->r.body + st->sent, n);
	st->sent += n;
	if (st->sent == st->r.len)
		*flags |= NGHTTP2_DATA_FLAG_EOF;
	return (nghttp2_ssize)n;
}

static int h2_respond(nghttp2_session *s, int32_t id, struct h2_stream *st)
{
	st->q.path = st->q.target;
	route(&st->q, &st->r);
	/* The status, the headers as HTTP/2 has them (lower case), the length. */
	nghttp2_nv nv[32];
	size_t n = 0;
	char status[8], length[32];
	snprintf(status, sizeof status, "%d", st->r.status);
	snprintf(length, sizeof length, "%zu", st->r.len);
	nv[n++] = (nghttp2_nv){ (uint8_t *)":status", (uint8_t *)status, 7, strlen(status), NGHTTP2_NV_FLAG_NONE };
	char *line = st->r.headers;
	for (char *end; n < 30 && (end = strstr(line, "\r\n")); line = end + 2) {
		char *colon = memchr(line, ':', (size_t)(end - line));
		if (!colon)
			break;
		for (char *k = line; k < colon; k++)
			*k = (char)tolower((unsigned char)*k);
		char *v = colon + 1;
		while (*v == ' ')
			v++;
		nv[n++] = (nghttp2_nv){ (uint8_t *)line, (uint8_t *)v, (size_t)(colon - line), (size_t)(end - v),
					NGHTTP2_NV_FLAG_NONE };
	}
	nv[n++] = (nghttp2_nv){ (uint8_t *)"content-length", (uint8_t *)length, 14, strlen(length),
				NGHTTP2_NV_FLAG_NONE };
	nghttp2_data_provider2 body = { .source.ptr = st, .read_callback = h2_body };
	return nghttp2_submit_response2(s, id, nv, n, strcmp(st->q.method, "HEAD") ? &body : 0);
}

static int h2_frame(nghttp2_session *s, const nghttp2_frame *f, void *user)
{
	(void)user;
	if ((f->hd.type != NGHTTP2_HEADERS && f->hd.type != NGHTTP2_DATA) || !(f->hd.flags & NGHTTP2_FLAG_END_STREAM))
		return 0;
	struct h2_stream *st = nghttp2_session_get_stream_user_data(s, f->hd.stream_id);
	return st && h2_respond(s, f->hd.stream_id, st) ? NGHTTP2_ERR_CALLBACK_FAILURE : 0;
}

static int h2_closed(nghttp2_session *s, int32_t id, uint32_t error, void *user)
{
	(void)error;
	(void)user;
	struct h2_stream *st = nghttp2_session_get_stream_user_data(s, id);
	if (st) {
		nghttp2_session_set_stream_user_data(s, id, 0);
		free(st->q.body);
		free(st->r.body);
		free(st);
	}
	return 0;
}

/* HTTP/2 on C, READ having been read of it already (by prior knowledge,
 * the start of its preface), and what C's buffer holds. */
static void serve_h2(struct conn *c, const unsigned char *read, size_t len)
{
	nghttp2_session_callbacks *cb;
	nghttp2_session *s;
	atomic_fetch_add(&h2_connections, 1);
	if (nghttp2_session_callbacks_new(&cb))
		return;
	nghttp2_session_callbacks_set_send_callback2(cb, h2_send);
	nghttp2_session_callbacks_set_on_begin_headers_callback(cb, h2_begin_headers);
	nghttp2_session_callbacks_set_on_header_callback(cb, h2_header);
	nghttp2_session_callbacks_set_on_data_chunk_recv_callback(cb, h2_data);
	nghttp2_session_callbacks_set_on_frame_recv_callback(cb, h2_frame);
	nghttp2_session_callbacks_set_on_stream_close_callback(cb, h2_closed);
	int failed = nghttp2_session_server_new(&s, cb, c);
	nghttp2_session_callbacks_del(cb);
	if (failed)
		return;
	nghttp2_settings_entry settings = { NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS, 100 };
	if (nghttp2_submit_settings(s, NGHTTP2_FLAG_NONE, &settings, 1) ||
	    (len && nghttp2_session_mem_recv2(s, read, len) < 0) ||
	    (c->have && nghttp2_session_mem_recv2(s, c->buf, c->have) < 0))
		goto done;
	c->have = 0;
	while (!nghttp2_session_send(s) && (nghttp2_session_want_read(s) || nghttp2_session_want_write(s))) {
		ssize_t n = conn_read(c, c->buf, sizeof c->buf);
		if (n <= 0 || nghttp2_session_mem_recv2(s, c->buf, (size_t)n) < 0)
			break;
	}
done:
	nghttp2_session_del(s);
}

/* A connection, as it begins: TLS (its first byte a handshake record's)
 * where the port takes it, HTTP/2 by ALPN; else HTTP/1.1. */
static void serve(struct conn *c)
{
	unsigned char first;
	struct pollfd p = { .fd = c->fd, .events = POLLIN };
	if (c->tls && !c->ssl && !c->have && poll(&p, 1, 30000) == 1 && recv(c->fd, &first, 1, MSG_PEEK) == 1 &&
	    first == 0x16) {
		const unsigned char *alpn = 0;
		unsigned int alpn_len = 0;
		if (!(c->ssl = SSL_new(c->tls)) || !SSL_set_fd(c->ssl, c->fd) || SSL_accept(c->ssl) != 1)
			return;
		SSL_get0_alpn_selected(c->ssl, &alpn, &alpn_len);
		if (alpn_len == 2 && !memcmp(alpn, "h2", 2)) {
			serve_h2(c, 0, 0);
			return;
		}
	}
	serve_http1(c);
}

static void *connection(void *arg)
{
	struct conn *c = arg;
	serve(c);
	if (c->ssl) {
		SSL_shutdown(c->ssl);
		SSL_free(c->ssl);
	}
	close(c->fd);
	free(c);
	return 0;
}

struct listener {
	int fd;
	SSL_CTX *tls;
};

static void *accept_loop(void *arg)
{
	struct listener *l = arg;
	for (;;) {
		int fd = accept4(l->fd, 0, 0, SOCK_CLOEXEC);
		if (fd < 0) {
			if (errno != EINTR && errno != ECONNABORTED && errno != EMFILE && errno != ENFILE)
				return 0;
			usleep(10000);
			continue;
		}
		struct conn *c = calloc(1, sizeof *c);
		pthread_t t;
		if (c) {
			c->fd = fd;
			c->tls = l->tls;
		}
		if (!c || pthread_create(&t, &detached, connection, c)) {
			close(fd);
			free(c);
		}
	}
}

static X509 *pem_certificate(const char *pem)
{
	BIO *b = BIO_new_mem_buf(pem, -1);
	X509 *x = b ? PEM_read_bio_X509(b, 0, 0, 0) : 0;
	BIO_free(b);
	return x;
}

static EVP_PKEY *pem_key(const char *pem)
{
	BIO *b = BIO_new_mem_buf(pem, -1);
	EVP_PKEY *k = b ? PEM_read_bio_PrivateKey(b, 0, 0, 0) : 0;
	BIO_free(b);
	return k;
}

/* HTTP/2 if the client offers it, else HTTP/1.1. */
static int select_alpn(SSL *ssl, const unsigned char **out, unsigned char *out_len, const unsigned char *in,
		       unsigned int in_len, void *arg)
{
	static const unsigned char ours[] = "\x02h2\x08http/1.1";
	(void)ssl;
	(void)arg;
	return SSL_select_next_proto((unsigned char **)out, out_len, ours, sizeof ours - 1, in, in_len) ==
			       OPENSSL_NPN_NEGOTIATED ?
		       SSL_TLSEXT_ERR_OK :
		       SSL_TLSEXT_ERR_NOACK;
}

/* The expired certificate, for the name it is for. */
static int pick_certificate(SSL *ssl, int *alert, void *arg)
{
	(void)alert;
	(void)arg;
	const char *name = SSL_get_servername(ssl, TLSEXT_NAMETYPE_host_name);
	if (name && !strcmp(name, "expired.example") &&
	    (SSL_use_certificate(ssl, expired_cert) != 1 || SSL_use_PrivateKey(ssl, server_key) != 1))
		return SSL_TLSEXT_ERR_ALERT_FATAL;
	return SSL_TLSEXT_ERR_OK;
}

/* A server's TLS: 1.2 or 1.3, HTTP/2 or HTTP/1.1 by ALPN; with MUTUAL,
 * only for clients with a certificate the test authority signed. */
static SSL_CTX *server_context(int mutual)
{
	SSL_CTX *ctx = SSL_CTX_new(TLS_server_method());
	X509 *cert = pem_certificate(SERVER_CERT), *ca = pem_certificate(CA_CERT);
	int ok = ctx && cert && ca && server_key && SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION) &&
		 SSL_CTX_use_certificate(ctx, cert) == 1 && SSL_CTX_use_PrivateKey(ctx, server_key) == 1;
	if (ok) {
		SSL_CTX_set_alpn_select_cb(ctx, select_alpn, 0);
		SSL_CTX_set_tlsext_servername_callback(ctx, pick_certificate);
		if (mutual) {
			ok = X509_STORE_add_cert(SSL_CTX_get_cert_store(ctx), ca) == 1;
			SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER | SSL_VERIFY_FAIL_IF_NO_PEER_CERT, 0);
		}
	}
	X509_free(cert);
	X509_free(ca);
	if (!ok) {
		SSL_CTX_free(ctx);
		return 0;
	}
	return ctx;
}

static int start_listener(int fd, SSL_CTX *tls)
{
	struct listener *l = malloc(sizeof *l);
	pthread_t t;
	if (!l || listen(fd, 128))
		return 0;
	l->fd = fd;
	l->tls = tls;
	return pthread_create(&t, &detached, accept_loop, l) == 0;
}

/* A TCP socket bound to ADDRESS (of FAMILY) and PORT (0: any), its port in
 * *BOUND; -1 if it cannot be. */
static int bound(int family, const char *address, int want, int *bound_port)
{
	struct sockaddr_storage a = { 0 };
	socklen_t len;
	int one = 1, fd = socket(family, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (family == AF_INET6) {
		struct sockaddr_in6 *a6 = (struct sockaddr_in6 *)&a;
		a6->sin6_family = AF_INET6;
		a6->sin6_port = htons((uint16_t)want);
		inet_pton(AF_INET6, address, &a6->sin6_addr);
		len = sizeof *a6;
		setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &one, sizeof one);
	} else {
		struct sockaddr_in *a4 = (struct sockaddr_in *)&a;
		a4->sin_family = AF_INET;
		a4->sin_port = htons((uint16_t)want);
		inet_pton(AF_INET, address, &a4->sin_addr);
		len = sizeof *a4;
	}
	if (fd < 0 || bind(fd, (struct sockaddr *)&a, len) || getsockname(fd, (struct sockaddr *)&a, &len)) {
		if (fd >= 0)
			close(fd);
		return -1;
	}
	*bound_port = ntohs(family == AF_INET6 ? ((struct sockaddr_in6 *)&a)->sin6_port :
						 ((struct sockaddr_in *)&a)->sin_port);
	return fd;
}

/* The compressed forms of TEXT, which the servers send. */
static int compress_text(void)
{
	size_t len = sizeof TEXT - 1;
	struct blob *zlibs[] = { &gzipped, &deflated };
	int bits[] = { 16 + 15, 15 };
	for (int i = 0; i < 2; i++) {
		z_stream z = { 0 };
		zlibs[i]->data = malloc(1024);
		if (!zlibs[i]->data || deflateInit2(&z, 9, Z_DEFLATED, bits[i], 8, Z_DEFAULT_STRATEGY) != Z_OK)
			return 0;
		z.next_in = (Bytef *)TEXT;
		z.avail_in = (uInt)len;
		z.next_out = zlibs[i]->data;
		z.avail_out = 1024;
		int done = deflate(&z, Z_FINISH) == Z_STREAM_END;
		zlibs[i]->len = z.total_out;
		deflateEnd(&z);
		if (!done)
			return 0;
	}
	brotli_text.len = BrotliEncoderMaxCompressedSize(len);
	zstd_text.len = ZSTD_compressBound(len);
	brotli_text.data = malloc(brotli_text.len);
	zstd_text.data = malloc(zstd_text.len);
	if (!brotli_text.data || !zstd_text.data ||
	    !BrotliEncoderCompress(BROTLI_DEFAULT_QUALITY, BROTLI_DEFAULT_WINDOW, BROTLI_MODE_TEXT, len,
				   (const uint8_t *)TEXT, &brotli_text.len, brotli_text.data))
		return 0;
	zstd_text.len = ZSTD_compress(zstd_text.data, zstd_text.len, TEXT, len, 3);
	return !ZSTD_isError(zstd_text.len);
}

static int start_servers(void)
{
	char cwd[256];
	int port6, fd4, fd6, mutual, local;
	pthread_attr_init(&detached);
	pthread_attr_setdetachstate(&detached, PTHREAD_CREATE_DETACHED);
	pthread_attr_setstacksize(&detached, 1 << 20);
	server_key = pem_key(SERVER_KEY);
	expired_cert = pem_certificate(EXPIRED_CERT);
	SSL_CTX *tls = server_context(0), *tls_mutual = server_context(1);
	CHECK("the servers' TLS", tls && tls_mutual && expired_cert, "OpenSSL refused the certificates");
	CHECK("the compressed responses", compress_text(), "a compressor failed");
	/* The main port, on IPv4's loopback address and IPv6's. */
	fd4 = bound(AF_INET, "127.0.0.1", 0, &port);
	fd6 = fd4 < 0 ? -1 : bound(AF_INET6, "::1", port, &port6);
	mutual = bound(AF_INET, "127.0.0.1", 0, &mutual_port);
	CHECK("the servers' ports", fd4 >= 0 && fd6 >= 0 && mutual >= 0, "IPv4 %d, IPv6 %d, mutual %d", fd4, fd6,
	      mutual);
	struct sockaddr_un a = { .sun_family = AF_UNIX };
	if (!getcwd(cwd, sizeof cwd) || strlen(cwd) + sizeof "/http.sock" > sizeof unix_path) {
		CHECK("the Unix domain socket's path", 0, "the directory's name is too long");
		return 0;
	}
	snprintf(unix_path, sizeof unix_path, "%s/http.sock", cwd);
	snprintf(a.sun_path, sizeof a.sun_path, "%s", unix_path);
	unlink(unix_path);
	local = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	CHECK("the Unix domain socket", local >= 0 && bind(local, (struct sockaddr *)&a, sizeof a) == 0, "%s: %s",
	      unix_path, strerror(errno));
	return tls && tls_mutual && fd4 >= 0 && fd6 >= 0 && mutual >= 0 && local >= 0 && start_listener(fd4, tls) &&
	       start_listener(fd6, tls) && start_listener(mutual, tls_mutual) && start_listener(local, 0);
}

/* A port of the loopback interface that nothing listens on. */
static int closed_port(void)
{
	int p = 0, fd = bound(AF_INET, "127.0.0.1", 0, &p);
	if (fd >= 0)
		close(fd);
	return p;
}

/* --- libcurl ----------------------------------------------------------------- */

/* Where the servers are. */
static char base[64], base6[64], named[64], secure[64], mutual[64];

static size_t collect(char *p, size_t size, size_t n, void *arg)
{
	struct buffer *b = arg;
	size_t len = size * n;
	char *more = realloc(b->data, b->len + len + 1);
	if (!more)
		return 0;
	b->data = more;
	memcpy(b->data + b->len, p, len);
	b->len += len;
	b->data[b->len] = 0;
	return len;
}

/* A handle whose transfers end within two minutes, whatever happens. */
static CURL *easy(void)
{
	CURL *h = curl_easy_init();
	if (h)
		curl_easy_setopt(h, CURLOPT_TIMEOUT, 120L);
	return h;
}

/* One that trusts the test authority. */
static CURL *secure_easy(void)
{
	struct curl_blob ca = { (void *)CA_CERT, sizeof CA_CERT - 1, CURL_BLOB_COPY };
	CURL *h = easy();
	if (h)
		curl_easy_setopt(h, CURLOPT_CAINFO_BLOB, &ca);
	return h;
}

/* Fetches URL with H as it is set up: the body into BODY (NUL-terminated),
 * the response's status into *STATUS. */
static CURLcode fetch(CURL *h, const char *url, struct buffer *body, long *status)
{
	empty(body);
	curl_easy_setopt(h, CURLOPT_URL, url);
	curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, collect);
	curl_easy_setopt(h, CURLOPT_WRITEDATA, body);
	CURLcode rc = curl_easy_perform(h);
	*status = 0;
	curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, status);
	if (!body->data)
		body->data = calloc(1, 1);
	return rc;
}

/* A one-off fetch with a new handle. */
static CURLcode get(const char *url, struct buffer *body, long *status)
{
	CURL *h = easy();
	CURLcode rc = fetch(h, url, body, status);
	curl_easy_cleanup(h);
	return rc;
}

static void check_version(void)
{
	curl_version_info_data *v = curl_version_info(CURLVERSION_NOW);
	printf("checks: %s\n", curl_version());
	CHECK("libcurl's version", !strcmp(v->version, VEDA_CURL_VERSION) && strstr(v->host, "-veda"), "%s, for %s",
	      v->version, v->host);
	int want = CURL_VERSION_SSL | CURL_VERSION_HTTP2 | CURL_VERSION_IPV6 | CURL_VERSION_LIBZ | CURL_VERSION_BROTLI |
		   CURL_VERSION_ZSTD | CURL_VERSION_PSL | CURL_VERSION_IDN | CURL_VERSION_ASYNCHDNS |
		   CURL_VERSION_UNIX_SOCKETS | CURL_VERSION_HSTS | CURL_VERSION_ALTSVC | CURL_VERSION_LARGEFILE |
		   CURL_VERSION_THREADSAFE | CURL_VERSION_HTTPS_PROXY;
	CHECK("libcurl's features", (v->features & want) == want, "missing %#x", want & ~v->features);
	CHECK("its TLS: OpenSSL", v->ssl_version && !strcmp(v->ssl_version, "OpenSSL/" VEDA_OPENSSL_VERSION), "%s",
	      v->ssl_version ? v->ssl_version : "none");
	/* Brotli's and Zstandard's are named as curl --version names them. */
	const char *none = "none";
	CHECK("the libraries it holds",
	      v->libz_version && !strcmp(v->libz_version, VEDA_ZLIB_VERSION) && v->nghttp2_version &&
		      !strcmp(v->nghttp2_version, VEDA_NGHTTP2_VERSION) && v->brotli_version &&
		      !strcmp(v->brotli_version, "brotli/" VEDA_BROTLI_VERSION) && v->zstd_version &&
		      !strcmp(v->zstd_version, "zstd/" VEDA_ZSTD_VERSION) && v->libidn &&
		      !strcmp(v->libidn, VEDA_LIBIDN2_VERSION) && strstr(curl_version(), "libpsl/" VEDA_LIBPSL_VERSION),
	      "zlib %s, nghttp2 %s, %s, %s, libidn2 %s; %s", v->libz_version ? v->libz_version : none,
	      v->nghttp2_version ? v->nghttp2_version : none, v->brotli_version ? v->brotli_version : none,
	      v->zstd_version ? v->zstd_version : none, v->libidn ? v->libidn : none, curl_version());
	int protocols = 0;
	static const char *const expected[] = { "http", "https", "ws", "wss", "ftp", "ftps", "file", 0 };
	for (const char *const *e = expected; *e; e++)
		for (const char *const *p = v->protocols; *p; p++)
			protocols += !strcmp(*p, *e);
	CHECK("its protocols", protocols == 7, "%d of the 7 expected", protocols);
}

/* The system's certificate authorities are libcurl's, and they alone: the
 * test authority's server is not trusted unless libcurl is told to. */
static void check_authorities(void)
{
	char *ca = 0, url[128];
	struct buffer body = { 0 };
	long status, verified = -1;
	CURL *h = easy();
	curl_easy_getinfo(h, CURLINFO_CAINFO, &ca);
	CHECK("the system's certificate authorities", ca && !strcmp(ca, "/system/etc/ssl/cert.pem"), "%s",
	      ca ? ca : "none");
	struct buffer pem = read_file("/system/etc/ssl/cert.pem");
	int count = 0;
	for (const char *p = pem.data; p && (p = strstr(p, "BEGIN CERTIFICATE")); p++)
		count++;
	CHECK("... the many of Mozilla's list", count > 100, "%d certificates", count);
	empty(&pem);
	snprintf(url, sizeof url, "%s/hello", secure);
	CURLcode rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_SSL_VERIFYRESULT, &verified);
	CHECK("... and no others", rc == CURLE_PEER_FAILED_VERIFICATION && verified != 0, "%s (verify result %ld)",
	      curl_easy_strerror(rc), verified);
	empty(&body);
	curl_easy_cleanup(h);
}

static void check_http(void)
{
	char url[256], ip[64] = "";
	struct buffer body = { 0 };
	long status, version = 0, connects = -1, redirects = 0;
	char *where = 0;
	CURL *h = easy();
	snprintf(url, sizeof url, "%s/hello", base);
	CURLcode rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_HTTP_VERSION, &version);
	curl_easy_getinfo(h, CURLINFO_PRIMARY_IP, &where);
	if (where)
		snprintf(ip, sizeof ip, "%s", where);
	CHECK("GET over IPv4", rc == CURLE_OK && status == 200 && !strcmp(body.data, "hello from Veda\n") &&
	      version == CURL_HTTP_VERSION_1_1 && !strcmp(ip, "127.0.0.1"), "%s, %ld, '%s' from %s",
	      curl_easy_strerror(rc), status, body.data, ip);
	rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_NUM_CONNECTS, &connects);
	CHECK("the connection kept for the next", rc == CURLE_OK && status == 200 && connects == 0,
	      "%s, %ld new connections", curl_easy_strerror(rc), connects);
	curl_easy_cleanup(h);

	snprintf(url, sizeof url, "%s/hello", base6);
	h = easy();
	rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_PRIMARY_IP, &where);
	CHECK("GET over IPv6", rc == CURLE_OK && status == 200 && where && !strcmp(where, "::1"), "%s, %ld from %s",
	      curl_easy_strerror(rc), status, where ? where : "nowhere");
	curl_easy_cleanup(h);
	snprintf(url, sizeof url, "%s/hello", named);
	rc = get(url, &body, &status);
	CHECK("a name looked up (localhost)", rc == CURLE_OK && status == 200, "%s, %ld", curl_easy_strerror(rc),
	      status);

	h = easy();
	curl_easy_setopt(h, CURLOPT_NOBODY, 1L);
	snprintf(url, sizeof url, "%s/hello", base);
	rc = fetch(h, url, &body, &status);
	CHECK("HEAD", rc == CURLE_OK && status == 200 && body.len == 0, "%s, %ld, %zu bytes", curl_easy_strerror(rc),
	      status, body.len);
	curl_easy_cleanup(h);

	h = easy();
	curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 1L);
	curl_easy_setopt(h, CURLOPT_MAXREDIRS, 5L);
	snprintf(url, sizeof url, "%s/redirect", base);
	rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_REDIRECT_COUNT, &redirects);
	CHECK("a redirect followed", rc == CURLE_OK && status == 200 && redirects == 1 &&
	      !strcmp(body.data, "hello from Veda\n"), "%s, %ld after %ld", curl_easy_strerror(rc), status, redirects);
	snprintf(url, sizeof url, "%s/redirect-loop", base);
	rc = fetch(h, url, &body, &status);
	CHECK("redirects without end", rc == CURLE_TOO_MANY_REDIRECTS, "%s", curl_easy_strerror(rc));
	curl_easy_cleanup(h);

	snprintf(url, sizeof url, "%s/status/404", base);
	rc = get(url, &body, &status);
	CHECK("a status of 404", rc == CURLE_OK && status == 404, "%s, %ld", curl_easy_strerror(rc), status);
	h = easy();
	curl_easy_setopt(h, CURLOPT_FAILONERROR, 1L);
	rc = fetch(h, url, &body, &status);
	CHECK("... an error, when asked", rc == CURLE_HTTP_RETURNED_ERROR, "%s", curl_easy_strerror(rc));
	curl_easy_cleanup(h);

	snprintf(url, sizeof url, "%s/auth", base);
	h = easy();
	curl_easy_setopt(h, CURLOPT_USERPWD, "veda:secret");
	rc = fetch(h, url, &body, &status);
	CHECK("basic authentication", rc == CURLE_OK && status == 200 && !strcmp(body.data, "welcome\n"), "%s, %ld",
	      curl_easy_strerror(rc), status);
	curl_easy_setopt(h, CURLOPT_USERPWD, "veda:wrong");
	rc = fetch(h, url, &body, &status);
	CHECK("... refused with the wrong password", rc == CURLE_OK && status == 401, "%s, %ld",
	      curl_easy_strerror(rc), status);
	curl_easy_cleanup(h);

	snprintf(url, sizeof url, "%s/chunked", base);
	rc = get(url, &body, &status);
	CHECK("a response in chunks", rc == CURLE_OK && !strcmp(body.data, TEXT), "%s, '%s'", curl_easy_strerror(rc),
	      body.data);

	snprintf(url, sizeof url, "http://127.0.0.1:%d/hello", closed_port());
	rc = get(url, &body, &status);
	CHECK("a port nobody listens on", rc == CURLE_COULDNT_CONNECT, "%s", curl_easy_strerror(rc));
	rc = get("http://nonexistent.invalid/", &body, &status);
	CHECK("a name nothing has", rc == CURLE_COULDNT_RESOLVE_HOST, "%s", curl_easy_strerror(rc));

	h = easy();
	curl_easy_setopt(h, CURLOPT_INTERFACE, "127.0.0.1");
	snprintf(url, sizeof url, "%s/hello", base);
	rc = fetch(h, url, &body, &status);
	CHECK("from a local address (CURLOPT_INTERFACE)", rc == CURLE_OK && status == 200, "%s, %ld",
	      curl_easy_strerror(rc), status);
	curl_easy_cleanup(h);

	h = easy();
	curl_easy_setopt(h, CURLOPT_UNIX_SOCKET_PATH, unix_path);
	rc = fetch(h, "http://localhost/hello", &body, &status);
	CHECK("over a Unix domain socket", rc == CURLE_OK && !strcmp(body.data, "hello from Veda\n"), "%s, '%s'",
	      curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);
	empty(&body);
}

/* A request's body, as libcurl reads it from the program. */
struct source {
	const unsigned char *data;
	size_t len, at;
};

static size_t supply(char *p, size_t size, size_t n, void *arg)
{
	struct source *s = arg;
	size_t k = s->len - s->at < size * n ? s->len - s->at : size * n;
	memcpy(p, s->data + s->at, k);
	s->at += k;
	return k;
}

static void check_uploads(void)
{
	char url[128];
	struct buffer body = { 0 };
	struct curl_header *method = 0;
	long status;
	size_t len = 3u << 20;
	unsigned char *data = malloc(len);
	for (size_t i = 0; i < len; i++)
		data[i] = big_byte(i * 7);
	snprintf(url, sizeof url, "%s/echo", base);

	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_POSTFIELDS, "name=Veda&os=yes");
	CURLcode rc = fetch(h, url, &body, &status);
	CURLHcode got = curl_easy_header(h, "X-Method", 0, CURLH_HEADER, -1, &method);
	CHECK("POST", rc == CURLE_OK && !strcmp(body.data, "name=Veda&os=yes") && got == CURLHE_OK &&
	      !strcmp(method->value, "POST"), "%s, '%s'", curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);

	/* Three megabytes: libcurl asks whether to send them first (Expect). */
	struct source source = { data, len, 0 };
	h = easy();
	curl_easy_setopt(h, CURLOPT_UPLOAD, 1L);
	curl_easy_setopt(h, CURLOPT_READFUNCTION, supply);
	curl_easy_setopt(h, CURLOPT_READDATA, &source);
	curl_easy_setopt(h, CURLOPT_INFILESIZE_LARGE, (curl_off_t)len);
	rc = fetch(h, url, &body, &status);
	got = curl_easy_header(h, "X-Method", 0, CURLH_HEADER, -1, &method);
	CHECK("PUT of 3 MiB", rc == CURLE_OK && body.len == len && !memcmp(body.data, data, len) && got == CURLHE_OK &&
	      !strcmp(method->value, "PUT"), "%s, %zu bytes back", curl_easy_strerror(rc), body.len);
	/* Of a size not said beforehand: in chunks. */
	source.at = 0;
	source.len = 100000;
	curl_easy_setopt(h, CURLOPT_INFILESIZE_LARGE, (curl_off_t)-1);
	rc = fetch(h, url, &body, &status);
	CHECK("PUT in chunks", rc == CURLE_OK && body.len == 100000 && !memcmp(body.data, data, 100000), "%s, %zu",
	      curl_easy_strerror(rc), body.len);
	curl_easy_cleanup(h);

	h = easy();
	curl_mime *form = curl_mime_init(h);
	curl_mimepart *part = curl_mime_addpart(form);
	curl_mime_name(part, "field");
	curl_mime_data(part, "a value of Veda's", CURL_ZERO_TERMINATED);
	curl_easy_setopt(h, CURLOPT_MIMEPOST, form);
	rc = fetch(h, url, &body, &status);
	CHECK("a form (multipart/form-data)", rc == CURLE_OK && strstr(body.data, "name=\"field\"") &&
	      strstr(body.data, "a value of Veda's"), "%s, '%s'", curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);
	curl_mime_free(form);
	free(data);
	empty(&body);
}

static void check_compression(void)
{
	static const char *const encodings[] = { "gzip", "deflate", "br", "zstd" };
	char url[128], name[64];
	struct buffer body = { 0 };
	long status;
	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_ACCEPT_ENCODING, "");
	for (int i = 0; i < 4; i++) {
		snprintf(url, sizeof url, "%s/%s", base, encodings[i]);
		snprintf(name, sizeof name, "Content-Encoding: %s", encodings[i]);
		CURLcode rc = fetch(h, url, &body, &status);
		CHECK(name, rc == CURLE_OK && !strcmp(body.data, TEXT), "%s, '%s'", curl_easy_strerror(rc), body.data);
	}
	snprintf(url, sizeof url, "%s/headers", base);
	CURLcode rc = fetch(h, url, &body, &status);
	CHECK("every encoding offered", rc == CURLE_OK && strstr(body.data, "Accept-Encoding: deflate, gzip, br, zstd"),
	      "%s", body.data);
	curl_easy_cleanup(h);
	empty(&body);
}

static void check_downloads(void)
{
	char url[128];
	struct buffer body = { 0 };
	long status;
	curl_off_t speed = 0;
	uint64_t n = 16u << 20, from = 10u << 20;
	snprintf(url, sizeof url, "%s/big?n=%llu", base, (unsigned long long)n);
	CURL *h = easy();
	double t0 = now();
	CURLcode rc = fetch(h, url, &body, &status);
	double took = now() - t0;
	curl_easy_getinfo(h, CURLINFO_SPEED_DOWNLOAD_T, &speed);
	CHECK("16 MiB, whole", rc == CURLE_OK && body.len == n && crc_of(body.data, body.len) == big_crc(0, n) &&
	      speed > 0, "%s, %zu bytes", curl_easy_strerror(rc), body.len);
	printf("checks: 16 MiB over HTTP on the loopback interface in %.2f s\n", took);
	curl_easy_setopt(h, CURLOPT_RESUME_FROM_LARGE, (curl_off_t)from);
	rc = fetch(h, url, &body, &status);
	CHECK("... and resumed from 10 MiB", rc == CURLE_OK && status == 206 && body.len == n - from &&
	      crc_of(body.data, body.len) == big_crc(from, n), "%s, %ld, %zu bytes", curl_easy_strerror(rc), status,
	      body.len);
	curl_easy_cleanup(h);

	/* A server that answers, then sends nothing. */
	h = easy();
	curl_easy_setopt(h, CURLOPT_TIMEOUT_MS, 1500L);
	snprintf(url, sizeof url, "%s/slow", base);
	t0 = now();
	rc = fetch(h, url, &body, &status);
	took = now() - t0;
	CHECK("a transfer that times out", rc == CURLE_OPERATION_TIMEDOUT && took > 1.4 && took < 15, "%s after %.2f s",
	      curl_easy_strerror(rc), took);
	curl_easy_cleanup(h);
	empty(&body);
}

static void check_proxy(void)
{
	char url[128];
	struct buffer body = { 0 };
	long status;
	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_PROXY, base);
	CURLcode rc = fetch(h, "http://far.example/headers", &body, &status);
	CHECK("through an HTTP proxy", rc == CURLE_OK && strstr(body.data, "GET http://far.example/headers HTTP/1.1"),
	      "%s, '%s'", curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);
	/* HTTPS through the proxy's tunnel (CONNECT), to the same server. */
	int before = atomic_load(&tunnels);
	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_PROXY, base);
	snprintf(url, sizeof url, "https://localhost:%d/tls", port);
	rc = fetch(h, url, &body, &status);
	CHECK("through a proxy's tunnel (CONNECT)", rc == CURLE_OK && !strncmp(body.data, "TLSv1.3 h2", 10) &&
	      atomic_load(&tunnels) == before + 1, "%s, '%s'", curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);
	empty(&body);
}

static void check_tls(void)
{
	char url[128], resolve[64], pin[128];
	struct buffer body = { 0 };
	long status, version = 0, verified = -1;
	snprintf(url, sizeof url, "%s/tls", secure);
	CURL *h = secure_easy();
	CURLcode rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_HTTP_VERSION, &version);
	CHECK("HTTPS: TLS 1.3, HTTP/2 by ALPN", rc == CURLE_OK && !strcmp(body.data, "TLSv1.3 h2 client=none\n") &&
	      version == CURL_HTTP_VERSION_2_0, "%s, '%s', HTTP version %ld", curl_easy_strerror(rc), body.data,
	      version);
	curl_easy_cleanup(h);

	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_SSLVERSION, (long)(CURL_SSLVERSION_TLSv1_2 | CURL_SSLVERSION_MAX_TLSv1_2));
	curl_easy_setopt(h, CURLOPT_HTTP_VERSION, CURL_HTTP_VERSION_1_1);
	rc = fetch(h, url, &body, &status);
	CHECK("TLS 1.2, HTTP/1.1", rc == CURLE_OK && !strcmp(body.data, "TLSv1.2 http/1.1 client=none\n"), "%s, '%s'",
	      curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);

	/* The name in the address must be the certificate's. */
	struct curl_slist *hosts = 0;
	snprintf(resolve, sizeof resolve, "wrong.example:%d:127.0.0.1", port);
	hosts = curl_slist_append(hosts, resolve);
	snprintf(resolve, sizeof resolve, "expired.example:%d:127.0.0.1", port);
	hosts = curl_slist_append(hosts, resolve);
	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_RESOLVE, hosts);
	snprintf(url, sizeof url, "https://wrong.example:%d/hello", port);
	rc = fetch(h, url, &body, &status);
	CHECK("a certificate for another name", rc == CURLE_PEER_FAILED_VERIFICATION, "%s", curl_easy_strerror(rc));
	snprintf(url, sizeof url, "https://expired.example:%d/hello", port);
	rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_SSL_VERIFYRESULT, &verified);
	CHECK("an expired certificate", rc == CURLE_PEER_FAILED_VERIFICATION && verified == X509_V_ERR_CERT_HAS_EXPIRED,
	      "%s (verify result %ld)", curl_easy_strerror(rc), verified);
	curl_easy_cleanup(h);
	curl_slist_free_all(hosts);

	/* Anything, when asked to verify nothing (curl -k). */
	h = easy();
	curl_easy_setopt(h, CURLOPT_SSL_VERIFYPEER, 0L);
	snprintf(url, sizeof url, "%s/hello", secure);
	rc = fetch(h, url, &body, &status);
	CHECK("unverified, when asked", rc == CURLE_OK && status == 200, "%s", curl_easy_strerror(rc));
	curl_easy_cleanup(h);

	/* The server's key pinned: its own, then another. */
	unsigned char *der = 0, digest[EVP_MAX_MD_SIZE];
	unsigned int dlen = 0;
	char encoded[64] = "";
	int der_len = i2d_PUBKEY(server_key, &der);
	if (der_len > 0 && EVP_Digest(der, (size_t)der_len, digest, &dlen, EVP_sha256(), 0))
		EVP_EncodeBlock((unsigned char *)encoded, digest, (int)dlen);
	OPENSSL_free(der);
	snprintf(pin, sizeof pin, "sha256//%s", encoded);
	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_PINNEDPUBLICKEY, pin);
	rc = fetch(h, url, &body, &status);
	CHECK("its public key pinned", rc == CURLE_OK, "%s (%s)", curl_easy_strerror(rc), pin);
	curl_easy_cleanup(h);
	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_PINNEDPUBLICKEY, "sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
	rc = fetch(h, url, &body, &status);
	CHECK("another key pinned", rc == CURLE_SSL_PINNEDPUBKEYNOTMATCH, "%s", curl_easy_strerror(rc));
	curl_easy_cleanup(h);

	/* The chain, as the server sent it. */
	struct curl_certinfo *chain = 0;
	int subject = 0;
	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_CERTINFO, 1L);
	rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_CERTINFO, &chain);
	if (chain && chain->num_of_certs > 0)
		for (struct curl_slist *s = chain->certinfo[0]; s; s = s->next)
			subject += !strncmp(s->data, "Subject:", 8) && strstr(s->data, "localhost");
	CHECK("the server's certificate (CURLINFO_CERTINFO)", rc == CURLE_OK && subject == 1, "%s, %d certificates",
	      curl_easy_strerror(rc), chain ? chain->num_of_certs : 0);
	curl_easy_cleanup(h);

	/* A server that asks for the client's certificate. */
	snprintf(url, sizeof url, "%s/tls", mutual);
	h = secure_easy();
	rc = fetch(h, url, &body, &status);
	CHECK("a client without a certificate refused", rc != CURLE_OK, "%s, '%s'", curl_easy_strerror(rc),
	      body.data);
	curl_easy_cleanup(h);
	struct curl_blob cert = { (void *)CLIENT_CERT, sizeof CLIENT_CERT - 1, CURL_BLOB_COPY };
	struct curl_blob key = { (void *)CLIENT_KEY, sizeof CLIENT_KEY - 1, CURL_BLOB_COPY };
	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_SSLCERT_BLOB, &cert);
	curl_easy_setopt(h, CURLOPT_SSLKEY_BLOB, &key);
	rc = fetch(h, url, &body, &status);
	CHECK("a client's certificate", rc == CURLE_OK && strstr(body.data, "client=Veda test client"), "%s, '%s'",
	      curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);
	empty(&body);
}

/* Runs the transfers of M to their end: how many succeeded. */
static int perform_all(CURLM *m)
{
	int running = 1, succeeded = 0, left;
	double until = now() + 180;
	while (running && now() < until) {
		if (curl_multi_perform(m, &running) != CURLM_OK)
			break;
		CURLMsg *msg;
		while ((msg = curl_multi_info_read(m, &left)))
			succeeded += msg->msg == CURLMSG_DONE && msg->data.result == CURLE_OK;
		if (running && curl_multi_poll(m, 0, 0, 1000, 0) != CURLM_OK)
			break;
	}
	return succeeded;
}

/* N transfers of URL at once (with HANDLE making each), each checked. */
static int many_at_once(int n, const char *url, size_t len, CURL *(*handle)(void), int pipewait, long *connects)
{
	CURLM *m = curl_multi_init();
	CURL *h[16];
	struct buffer b[16] = { 0 };
	int right = 0;
	unsigned long crc = big_crc(0, len);
	curl_multi_setopt(m, CURLMOPT_PIPELINING, CURLPIPE_MULTIPLEX);
	for (int i = 0; i < n; i++) {
		h[i] = handle();
		curl_easy_setopt(h[i], CURLOPT_URL, url);
		curl_easy_setopt(h[i], CURLOPT_WRITEFUNCTION, collect);
		curl_easy_setopt(h[i], CURLOPT_WRITEDATA, &b[i]);
		curl_easy_setopt(h[i], CURLOPT_PIPEWAIT, (long)pipewait);
		curl_multi_add_handle(m, h[i]);
	}
	int succeeded = perform_all(m);
	*connects = 0;
	for (int i = 0; i < n; i++) {
		long c = 0;
		curl_easy_getinfo(h[i], CURLINFO_NUM_CONNECTS, &c);
		*connects += c;
		right += b[i].len == len && crc_of(b[i].data, len) == crc;
		curl_multi_remove_handle(m, h[i]);
		curl_easy_cleanup(h[i]);
		empty(&b[i]);
	}
	curl_multi_cleanup(m);
	return succeeded == n ? right : 0;
}

static void *wake(void *m)
{
	usleep(200000);
	curl_multi_wakeup(m);
	return 0;
}

static void check_multi(void)
{
	char url[128];
	long connects = 0;
	snprintf(url, sizeof url, "%s/big?n=%d", base, 1 << 18);
	int right = many_at_once(16, url, 1 << 18, easy, 0, &connects);
	CHECK("16 transfers at once (the multi interface)", right == 16, "%d right", right);

	/* A wait for nothing, which another thread ends. */
	CURLM *m = curl_multi_init();
	pthread_t t;
	double t0 = now();
	int woken = pthread_create(&t, 0, wake, m) == 0 && curl_multi_poll(m, 0, 0, 10000, 0) == CURLM_OK;
	double took = now() - t0;
	pthread_join(t, 0);
	CHECK("a wait woken by another thread (curl_multi_wakeup)", woken && took < 5, "%.2f s", took);
	curl_multi_cleanup(m);
}

static void *worker(void *arg)
{
	int *right = arg;
	const char *bases[] = { base, base6, named };
	char url[128];
	struct buffer body = { 0 };
	long status;
	CURL *h = easy();
	for (int i = 0; i < 25; i++) {
		snprintf(url, sizeof url, "%s/hello", bases[i % 3]);
		*right += fetch(h, url, &body, &status) == CURLE_OK && status == 200 &&
			  !strcmp(body.data, "hello from Veda\n");
	}
	curl_easy_cleanup(h);
	empty(&body);
	return 0;
}

static void check_threads(void)
{
	pthread_t t[8];
	int right[8] = { 0 }, total = 0, started = 0;
	for (int i = 0; i < 8; i++)
		started += pthread_create(&t[i], 0, worker, &right[i]) == 0;
	for (int i = 0; i < started; i++) {
		pthread_join(t[i], 0);
		total += right[i];
	}
	CHECK("8 threads, each with its own transfers", started == 8 && total == 8 * 25, "%d of %d right", total,
	      8 * 25);
}

static void check_http2(void)
{
	char url[128];
	struct buffer body = { 0 };
	long status, version = 0, connects = 0;
	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_HTTP_VERSION, CURL_HTTP_VERSION_2_PRIOR_KNOWLEDGE);
	snprintf(url, sizeof url, "%s/tls", base);
	CURLcode rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_HTTP_VERSION, &version);
	CHECK("HTTP/2 by prior knowledge", rc == CURLE_OK && !strcmp(body.data, "plain h2c\n") &&
	      version == CURL_HTTP_VERSION_2_0, "%s, '%s'", curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);

	/* Eight streams at once, on the one connection the first opens. */
	int before = atomic_load(&h2_connections);
	snprintf(url, sizeof url, "%s/big?n=%d", secure, 1 << 20);
	int right = many_at_once(8, url, 1 << 20, secure_easy, 1, &connects);
	int opened = atomic_load(&h2_connections) - before;
	CHECK("8 streams at once on one connection", right == 8 && connects == 1 && opened == 1,
	      "%d right, %ld connections made, %d served", right, connects, opened);

	/* Flow control: more than a window, each way. */
	h = secure_easy();
	snprintf(url, sizeof url, "%s/big?n=%d", secure, 8 << 20);
	rc = fetch(h, url, &body, &status);
	CHECK("8 MiB down over HTTP/2", rc == CURLE_OK && body.len == 8u << 20 &&
	      crc_of(body.data, body.len) == big_crc(0, 8u << 20), "%s, %zu bytes", curl_easy_strerror(rc), body.len);
	size_t len = 1u << 20;
	char *data = malloc(len);
	for (size_t i = 0; i < len; i++)
		data[i] = (char)big_byte(i + 3);
	curl_easy_setopt(h, CURLOPT_POSTFIELDS, data);
	curl_easy_setopt(h, CURLOPT_POSTFIELDSIZE_LARGE, (curl_off_t)len);
	snprintf(url, sizeof url, "%s/echo", secure);
	rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_HTTP_VERSION, &version);
	CHECK("1 MiB up over HTTP/2", rc == CURLE_OK && version == CURL_HTTP_VERSION_2_0 && body.len == len &&
	      !memcmp(body.data, data, len), "%s, %zu bytes back", curl_easy_strerror(rc), body.len);
	free(data);
	curl_easy_cleanup(h);
	empty(&body);
}

static void check_cookies(void)
{
	char url[128], resolve[64];
	struct buffer body = { 0 };
	struct curl_slist *hosts = 0, *cookies = 0;
	long status;
	snprintf(resolve, sizeof resolve, "www.example.co.uk:%d:127.0.0.1", port);
	hosts = curl_slist_append(hosts, resolve);
	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_RESOLVE, hosts);
	curl_easy_setopt(h, CURLOPT_COOKIEFILE, "");
	snprintf(url, sizeof url, "http://www.example.co.uk:%d/cookies/set", port);
	CURLcode rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_COOKIELIST, &cookies);
	int plain = 0, site = 0, suffix = 0;
	for (struct curl_slist *c = cookies; c; c = c->next) {
		plain += strstr(c->data, "\tplain\t1") != 0;
		site += strstr(c->data, "\tsite\t1") != 0;
		suffix += strstr(c->data, "\tsuffix\t") != 0;
	}
	curl_slist_free_all(cookies);
	CHECK("cookies kept", rc == CURLE_OK && plain == 1 && site == 1, "%s, plain %d, site %d",
	      curl_easy_strerror(rc), plain, site);
	CHECK("none for a public suffix (the Public Suffix List)", suffix == 0, "%d for co.uk", suffix);
	snprintf(url, sizeof url, "http://www.example.co.uk:%d/cookies", port);
	rc = fetch(h, url, &body, &status);
	CHECK("... and sent back", rc == CURLE_OK && strstr(body.data, "plain=1") && strstr(body.data, "site=1") &&
	      !strstr(body.data, "suffix"), "%s, '%s'", curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(h);
	curl_slist_free_all(hosts);

	/* Shared between handles. */
	CURLSH *share = curl_share_init();
	curl_share_setopt(share, CURLSHOPT_SHARE, CURL_LOCK_DATA_COOKIE);
	CURL *a = easy(), *b = easy();
	curl_easy_setopt(a, CURLOPT_SHARE, share);
	curl_easy_setopt(b, CURLOPT_SHARE, share);
	curl_easy_setopt(a, CURLOPT_COOKIEFILE, "");
	curl_easy_setopt(b, CURLOPT_COOKIEFILE, "");
	snprintf(url, sizeof url, "%s/cookies/set", base);
	fetch(a, url, &body, &status);
	snprintf(url, sizeof url, "%s/cookies", base);
	rc = fetch(b, url, &body, &status);
	CHECK("cookies shared (curl_share)", rc == CURLE_OK && !strcmp(body.data, "plain=1\n"), "%s, '%s'",
	      curl_easy_strerror(rc), body.data);
	curl_easy_cleanup(a);
	curl_easy_cleanup(b);
	curl_share_cleanup(share);
	empty(&body);
}

/* Strict Transport Security: what an HTTPS response asks for, the next
 * request of plain HTTP to its host is HTTPS, through the file it is kept
 * in. */
static void check_hsts(void)
{
	char url[128];
	struct buffer body = { 0 };
	char *effective = 0;
	long status;
	unlink("hsts.txt");
	CURL *h = secure_easy();
	curl_easy_setopt(h, CURLOPT_HSTS_CTRL, CURLHSTS_ENABLE);
	curl_easy_setopt(h, CURLOPT_HSTS, "hsts.txt");
	snprintf(url, sizeof url, "%s/hsts", secure);
	CURLcode rc = fetch(h, url, &body, &status);
	curl_easy_cleanup(h);
	struct buffer kept = read_file("hsts.txt");
	CHECK("HSTS: a host remembered", rc == CURLE_OK && kept.data && strstr(kept.data, "localhost"), "%s, '%s'",
	      curl_easy_strerror(rc), kept.data ? kept.data : "no file");
	empty(&kept);
	h = secure_easy();
	curl_easy_setopt(h, CURLOPT_HSTS_CTRL, CURLHSTS_ENABLE);
	curl_easy_setopt(h, CURLOPT_HSTS, "hsts.txt");
	snprintf(url, sizeof url, "%s/tls", named);
	rc = fetch(h, url, &body, &status);
	curl_easy_getinfo(h, CURLINFO_EFFECTIVE_URL, &effective);
	CHECK("... and plain HTTP to it made HTTPS", rc == CURLE_OK && !strncmp(body.data, "TLSv1.", 6) && effective &&
	      !strncmp(effective, "https://", 8), "%s, '%s' from %s", curl_easy_strerror(rc), body.data,
	      effective ? effective : "nowhere");
	curl_easy_cleanup(h);
	empty(&body);
}

/* What a WebSocket sends back, as libcurl receives it. */
static int ws_echo(CURL *h, const unsigned char *message, size_t len)
{
	curl_socket_t fd = CURL_SOCKET_BAD;
	unsigned char *back = malloc(len + 1);
	size_t at = 0, got = 0;
	double until = now() + 30;
	curl_easy_getinfo(h, CURLINFO_ACTIVESOCKET, &fd);
	while (back && at < len && now() < until) {
		size_t sent = 0;
		CURLcode rc = curl_ws_send(h, message + at, len - at, &sent, 0, CURLWS_BINARY);
		at += sent;
		if (rc == CURLE_AGAIN) {
			struct pollfd p = { .fd = fd, .events = POLLOUT };
			poll(&p, 1, 100);
		} else if (rc != CURLE_OK) {
			break;
		}
	}
	while (back && at == len && got < len && now() < until) {
		size_t n = 0;
		const struct curl_ws_frame *meta = 0;
		CURLcode rc = curl_ws_recv(h, back + got, len - got, &n, &meta);
		got += n;
		if (rc == CURLE_AGAIN) {
			struct pollfd p = { .fd = fd, .events = POLLIN };
			poll(&p, 1, 100);
		} else if (rc != CURLE_OK) {
			break;
		}
	}
	int same = back && got == len && !memcmp(back, message, len);
	free(back);
	return same;
}

static void check_websockets(void)
{
	static const char *const servers[] = { "ws://127.0.0.1:%d/ws", "wss://localhost:%d/ws" };
	static const char *const names[] = { "a WebSocket", "a WebSocket over TLS" };
	size_t len = 70000;
	unsigned char *message = malloc(len);
	for (size_t i = 0; i < len; i++)
		message[i] = big_byte(i + 11);
	for (int i = 0; i < 2; i++) {
		char url[128];
		snprintf(url, sizeof url, servers[i], port);
		CURL *h = secure_easy();
		curl_easy_setopt(h, CURLOPT_URL, url);
		curl_easy_setopt(h, CURLOPT_CONNECT_ONLY, 2L);
		CURLcode rc = curl_easy_perform(h);
		int small = rc == CURLE_OK && ws_echo(h, (const unsigned char *)"hello over a WebSocket", 22);
		int large = rc == CURLE_OK && ws_echo(h, message, len);
		CHECK(names[i], small && large, "%s, a short message %s, 70000 bytes %s", curl_easy_strerror(rc),
		      small ? "echoed" : "lost", large ? "echoed" : "lost");
		curl_easy_cleanup(h);
	}
	free(message);
}

static void check_files(void)
{
	char cwd[256], url[512];
	struct buffer body = { 0 };
	long status;
	if (!getcwd(cwd, sizeof cwd))
		cwd[0] = 0;
	static const char local[] = "a file of the scratch directory\n";
	write_file("local.txt", local, sizeof local - 1);
	snprintf(url, sizeof url, "file://%s/local.txt", cwd);
	CURLcode rc = get(url, &body, &status);
	CHECK("file:// read", rc == CURLE_OK && !strcmp(body.data, local), "%s, '%s'",
	      curl_easy_strerror(rc), body.data);
	rc = get("file:///system/etc/version", &body, &status);
	CHECK("file:// of the system image", rc == CURLE_OK && !strncmp(body.data, "Veda ", 5), "%s, '%s'",
	      curl_easy_strerror(rc), body.data);
	static const char written_text[] = "written by libcurl\n";
	struct source source = { (const unsigned char *)written_text, sizeof written_text - 1, 0 };
	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_UPLOAD, 1L);
	curl_easy_setopt(h, CURLOPT_READFUNCTION, supply);
	curl_easy_setopt(h, CURLOPT_READDATA, &source);
	snprintf(url, sizeof url, "file://%s/written.txt", cwd);
	rc = fetch(h, url, &body, &status);
	curl_easy_cleanup(h);
	struct buffer written = read_file("written.txt");
	CHECK("file:// written", rc == CURLE_OK && written.data && !strcmp(written.data, written_text),
	      "%s, '%s'", curl_easy_strerror(rc), written.data ? written.data : "no file");
	empty(&written);
	empty(&body);
}

/* URLs: parsed and put together, international domain names (IDNA2008,
 * by libidn2, in the locale's encoding: UTF-8) to the names DNS knows and
 * back. */
static void check_urls(void)
{
	char *got = 0, url[128], resolve[96];
	CURLU *u = curl_url();
	CURLUcode rc = curl_url_set(u, CURLUPART_URL, "https://example.com/a/./b/../c?x=1#here", 0);
	if (rc == CURLUE_OK)
		rc = curl_url_get(u, CURLUPART_URL, &got, 0);
	CHECK("a URL normalised", rc == CURLUE_OK && !strcmp(got, "https://example.com/a/c?x=1#here"), "%s",
	      rc == CURLUE_OK ? got : curl_url_strerror(rc));
	curl_free(got);
	got = 0;
	rc = curl_url_set(u, CURLUPART_URL, "https://bücher.example/", 0);
	if (rc == CURLUE_OK)
		rc = curl_url_get(u, CURLUPART_HOST, &got, CURLU_PUNYCODE);
	CHECK("an international name to punycode", rc == CURLUE_OK && !strcmp(got, "xn--bcher-kva.example"), "%s",
	      rc == CURLUE_OK ? got : curl_url_strerror(rc));
	curl_free(got);
	got = 0;
	rc = curl_url_set(u, CURLUPART_URL, "https://xn--bcher-kva.example/", 0);
	if (rc == CURLUE_OK)
		rc = curl_url_get(u, CURLUPART_HOST, &got, CURLU_PUNY2IDN);
	CHECK("... and back", rc == CURLUE_OK && !strcmp(got, "bücher.example"), "%s",
	      rc == CURLUE_OK ? got : curl_url_strerror(rc));
	curl_free(got);
	CHECK("a name with a space refused", curl_url_set(u, CURLUPART_URL, "http://exa mple.com/", 0) != CURLUE_OK,
	      "taken");
	curl_url_cleanup(u);

	/* A request to an international name: the server is asked for the
	   name DNS knows. */
	struct buffer body = { 0 };
	struct curl_slist *hosts = 0;
	long status;
	snprintf(resolve, sizeof resolve, "xn--bcher-kva.example:%d:127.0.0.1", port);
	hosts = curl_slist_append(hosts, resolve);
	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_RESOLVE, hosts);
	snprintf(url, sizeof url, "http://bücher.example:%d/headers", port);
	CURLcode code = fetch(h, url, &body, &status);
	snprintf(resolve, sizeof resolve, "Host: xn--bcher-kva.example:%d\r\n", port);
	CHECK("a request to an international name", code == CURLE_OK && strstr(body.data, resolve), "%s, '%s'",
	      curl_easy_strerror(code), body.data);
	char *escaped = curl_easy_escape(h, "a b&c/ü", 0);
	CHECK("escaping", escaped && !strcmp(escaped, "a%20b%26c%2F%C3%BC"), "%s", escaped ? escaped : "failed");
	curl_free(escaped);
	curl_easy_cleanup(h);
	curl_slist_free_all(hosts);
	empty(&body);
	CHECK("dates", curl_getdate("Wed, 21 Oct 2015 07:28:00 GMT", 0) == 1445412480, "%lld",
	      (long long)curl_getdate("Wed, 21 Oct 2015 07:28:00 GMT", 0));
}

/* The host machine's web server, through QEMU's NAT: a page, and a range
 * of a file. The network comes up after the system starts (its card is the
 * driver VM's): until it does, connections fail at once. */
static void check_host(const char *host, int tcp_port)
{
	char url[128];
	struct buffer body = { 0 };
	long status = 0;
	CURLcode rc = CURLE_COULDNT_CONNECT;
	snprintf(url, sizeof url, "http://%s:%d/hello", host, tcp_port);
	for (int i = 0; i < 240 && rc == CURLE_COULDNT_CONNECT; i++) {
		rc = get(url, &body, &status);
		if (rc == CURLE_COULDNT_CONNECT)
			usleep(250000);
	}
	CHECK("the host's web server", rc == CURLE_OK && !strcmp(body.data, "hello from the host\n"), "%s, '%s'",
	      curl_easy_strerror(rc), body.data);
	CURL *h = easy();
	curl_easy_setopt(h, CURLOPT_RANGE, "8-15");
	snprintf(url, sizeof url, "http://%s:%d/media/tone.wav", host, tcp_port);
	rc = fetch(h, url, &body, &status);
	CHECK("... a range of its file", rc == CURLE_OK && status == 206 && !strcmp(body.data, "WAVEfmt "),
	      "%s, %ld, '%s'", curl_easy_strerror(rc), status, body.data);
	curl_easy_cleanup(h);
	empty(&body);
}

/* --- the curl program ------------------------------------------------------------ */

static const char CURL_PROGRAM[] = "/system/bin/curl";

/* Starts the curl program with ARGS (after `-q`, which keeps any .curlrc
 * out of it) and the environment ENV, its standard output into out.txt and
 * its error into err.txt: its process id, or -1. */
static pid_t start_curl(const char *const *args, char *const *env)
{
	char *argv[64];
	int n = 0;
	argv[n++] = "curl";
	argv[n++] = "-q";
	for (; *args && n < 63; args++)
		argv[n++] = (char *)*args;
	argv[n] = 0;
	posix_spawn_file_actions_t actions;
	posix_spawn_file_actions_init(&actions);
	posix_spawn_file_actions_addopen(&actions, 1, "out.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	posix_spawn_file_actions_addopen(&actions, 2, "err.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	pid_t pid;
	int e = posix_spawn(&pid, CURL_PROGRAM, &actions, 0, argv, env);
	posix_spawn_file_actions_destroy(&actions);
	return e ? -1 : pid;
}

/* Its exit status, 128 and the signal if one ended it, or -1. */
static int finish(pid_t pid)
{
	int status;
	if (pid < 0)
		return -1;
	while (waitpid(pid, &status, 0) < 0)
		if (errno != EINTR)
			return -1;
	return WIFEXITED(status) ? WEXITSTATUS(status) : WIFSIGNALED(status) ? 128 + WTERMSIG(status) : -1;
}

/* The curl program, run with the arguments given and the environment's:
 * its exit status; what it printed is in OUT, its errors in ERRORS, and
 * their first line in ERR, for the messages. */
static struct buffer out, errors;
static char err[256];

static int run_curl_in(char *const *env, const char *const *args)
{
	int status = finish(start_curl(args, env));
	empty(&out);
	empty(&errors);
	out = read_file("out.txt");
	errors = read_file("err.txt");
	if (!out.data)
		out.data = calloc(1, 1);
	if (!errors.data)
		errors.data = calloc(1, 1);
	snprintf(err, sizeof err, "%s", errors.data);
	err[strcspn(err, "\n")] = 0;
	return status;
}

#define run_curl(...) run_curl_in(environ, (const char *const[]){ __VA_ARGS__, 0 })

static void check_program(const char *host, int host_port)
{
	char url[256], other[256], resolve[96], host_header[128];
	int status;
	write_file("ca.pem", CA_CERT, sizeof CA_CERT - 1);
	write_file("client.pem", CLIENT_CERT, sizeof CLIENT_CERT - 1);
	write_file("client.key", CLIENT_KEY, sizeof CLIENT_KEY - 1);

	status = run_curl("--version");
	CHECK("curl --version",
	      status == 0 && !strncmp(out.data, "curl " VEDA_CURL_VERSION " (", 6 + sizeof VEDA_CURL_VERSION) &&
		      strstr(out.data, "libcurl/" VEDA_CURL_VERSION " OpenSSL/" VEDA_OPENSSL_VERSION) &&
		      strstr(out.data, "nghttp2/" VEDA_NGHTTP2_VERSION) && strstr(out.data, "libidn2/" VEDA_LIBIDN2_VERSION) &&
		      strstr(out.data, "\nFeatures: ") && strstr(out.data, " HTTP2 ") && strstr(out.data, " PSL "),
	      "%d, '%s' %s", status, out.data, err);

	snprintf(url, sizeof url, "%s/hello", base);
	status = run_curl("-sS", url);
	CHECK("curl URL", status == 0 && !strcmp(out.data, "hello from Veda\n"), "%d, '%s' %s", status, out.data, err);
	status = run_curl("-sSI", url);
	CHECK("curl -I", status == 0 && !strncmp(out.data, "HTTP/1.1 200 OK\r\n", 17), "%d, '%s' %s", status, out.data,
	      err);
	snprintf(url, sizeof url, "%s/status/404", base);
	status = run_curl("-fsS", url);
	CHECK("curl -f, of a 404", status == 22, "%d %s", status, err);
	snprintf(url, sizeof url, "%s/redirect", base);
	status = run_curl("-sSL", url);
	CHECK("curl -L", status == 0 && !strcmp(out.data, "hello from Veda\n"), "%d, '%s' %s", status, out.data, err);
	snprintf(url, sizeof url, "%s/zstd", base);
	status = run_curl("-sS", "--compressed", url);
	CHECK("curl --compressed", status == 0 && !strcmp(out.data, TEXT), "%d, '%s' %s", status, out.data, err);

	/* Files: written whole, resumed, named after the URL. */
	uint64_t n = 16u << 20, part = 3u << 20;
	snprintf(url, sizeof url, "%s/big?n=%llu", base, (unsigned long long)n);
	status = run_curl("-sS", "-o", "big.bin", url);
	struct buffer file = read_file("big.bin");
	CHECK("curl -o", status == 0 && file.len == n && crc_of(file.data, file.len) == big_crc(0, n), "%d, %zu bytes %s",
	      status, file.len, err);
	file.len = part;
	write_file("part.bin", file.data, part);
	empty(&file);
	status = run_curl("-sS", "-C", "-", "-o", "part.bin", url);
	file = read_file("part.bin");
	CHECK("curl -C -, from 3 MiB", status == 0 && file.len == n && crc_of(file.data, file.len) == big_crc(0, n),
	      "%d, %zu bytes %s", status, file.len, err);
	empty(&file);
	snprintf(url, sizeof url, "%s/hello", base);
	status = run_curl("-sS", "-O", "--output-dir", "downloads/new", "--create-dirs", url);
	file = read_file("downloads/new/hello");
	CHECK("curl -O --create-dirs", status == 0 && file.data && !strcmp(file.data, "hello from Veda\n"), "%d %s",
	      status, err);
	empty(&file);
	snprintf(url, sizeof url, "%s/time", base);
	status = run_curl("-sS", "-R", "-o", "timed.txt", url);
	struct stat st = { 0 };
	CHECK("curl -R: the server's time", status == 0 && stat("timed.txt", &st) == 0 && st.st_mtime == 1445412480,
	      "%d, %lld %s", status, (long long)st.st_mtime, err);
	/* Veda's files keep no extended attributes: it says so, and writes the
	   file. */
	snprintf(url, sizeof url, "%s/hello", base);
	status = run_curl("-S", "--no-progress-meter", "--xattr", "-o", "attributes.txt", url);
	file = read_file("attributes.txt");
	CHECK("curl --xattr: the file, without its attributes", status == 0 && file.data &&
	      !strcmp(file.data, "hello from Veda\n") && strstr(errors.data, "extended attributes"), "%d '%s'", status,
	      errors.data);
	empty(&file);

	/* Uploads. */
	unsigned char data[200000];
	for (size_t i = 0; i < sizeof data; i++)
		data[i] = big_byte(i * 5);
	write_file("upload.bin", data, sizeof data);
	snprintf(url, sizeof url, "%s/echo", base);
	status = run_curl("-sS", "-T", "upload.bin", "-o", "echoed.bin", url);
	file = read_file("echoed.bin");
	CHECK("curl -T", status == 0 && file.len == sizeof data && !memcmp(file.data, data, sizeof data), "%d, %zu %s",
	      status, file.len, err);
	empty(&file);
	static const char form[] = "line one\nline two\n";
	write_file("form.txt", form, sizeof form - 1);
	status = run_curl("-sS", "--data-binary", "@form.txt", url);
	CHECK("curl --data-binary @FILE", status == 0 && !strcmp(out.data, form), "%d, '%s' %s",
	      status, out.data, err);

	/* TLS and HTTP/2. */
	snprintf(url, sizeof url, "%s/tls", secure);
	status = run_curl("-sS", "--cacert", "ca.pem", url);
	CHECK("curl https://", status == 0 && !strcmp(out.data, "TLSv1.3 h2 client=none\n"), "%d, '%s' %s", status,
	      out.data, err);
	status = run_curl("-sS", url);
	CHECK("curl https:// of an authority not trusted", status == 60, "%d %s", status, err);
	status = run_curl("-sS", "--cacert", "ca.pem", "--tls-max", "1.2", "--http1.1", url);
	CHECK("curl --tls-max 1.2 --http1.1", status == 0 && !strcmp(out.data, "TLSv1.2 http/1.1 client=none\n"),
	      "%d, '%s' %s", status, out.data, err);
	status = run_curl("-sS", "--cacert", "ca.pem", "-o", "/dev/null", "-w", "%{http_version}", url);
	CHECK("curl -w %{http_version}", status == 0 && !strcmp(out.data, "2"), "%d, '%s' %s", status, out.data, err);
	snprintf(url, sizeof url, "%s/tls", base);
	status = run_curl("-sS", "--http2-prior-knowledge", url);
	CHECK("curl --http2-prior-knowledge", status == 0 && !strcmp(out.data, "plain h2c\n"), "%d, '%s' %s", status,
	      out.data, err);
	snprintf(url, sizeof url, "%s/tls", mutual);
	status = run_curl("-sS", "--cacert", "ca.pem", "--cert", "client.pem", "--key", "client.key", url);
	CHECK("curl --cert", status == 0 && strstr(out.data, "client=Veda test client"), "%d, '%s' %s", status,
	      out.data, err);

	/* Elsewhere than TCP. */
	status = run_curl("-sS", "--unix-socket", unix_path, "http://localhost/hello");
	CHECK("curl --unix-socket", status == 0 && !strcmp(out.data, "hello from Veda\n"), "%d, '%s' %s", status,
	      out.data, err);
	status = run_curl("-sS", "file:///system/etc/version");
	CHECK("curl file://", status == 0 && !strncmp(out.data, "Veda ", 5), "%d, '%s' %s", status, out.data, err);

	/* An international name, in the locale's encoding (UTF-8). */
	snprintf(resolve, sizeof resolve, "xn--bcher-kva.example:%d:127.0.0.1", port);
	snprintf(url, sizeof url, "http://bücher.example:%d/headers", port);
	status = run_curl("-sS", "--resolve", resolve, url);
	snprintf(host_header, sizeof host_header, "Host: xn--bcher-kva.example:%d", port);
	CHECK("curl of an international name", status == 0 && strstr(out.data, host_header), "%d, '%s' %s", status,
	      out.data, err);

	/* Cookies and HSTS, kept in files between runs. */
	snprintf(url, sizeof url, "%s/cookies/set", base);
	status = run_curl("-sS", "-c", "jar.txt", "-o", "/dev/null", url);
	snprintf(url, sizeof url, "%s/cookies", base);
	int again = run_curl("-sS", "-b", "jar.txt", url);
	CHECK("curl -c, then -b", status == 0 && again == 0 && !strcmp(out.data, "plain=1\n"), "%d %d, '%s' %s", status,
	      again, out.data, err);
	unlink("hsts.txt");
	snprintf(url, sizeof url, "%s/hsts", secure);
	status = run_curl("-sS", "--cacert", "ca.pem", "--hsts", "hsts.txt", "-o", "/dev/null", url);
	snprintf(url, sizeof url, "%s/tls", named);
	again = run_curl("-sS", "--cacert", "ca.pem", "--hsts", "hsts.txt", url);
	CHECK("curl --hsts", status == 0 && again == 0 && !strncmp(out.data, "TLSv1.", 6), "%d %d, '%s' %s", status,
	      again, out.data, err);

	/* Several at once, and a configuration file. */
	snprintf(url, sizeof url, "%s/hello", base);
	snprintf(other, sizeof other, "%s/chunked", base);
	status = run_curl("-sS", "-Z", "-o", "one.txt", url, "-o", "two.txt", other);
	struct buffer one = read_file("one.txt"), two = read_file("two.txt");
	CHECK("curl -Z", status == 0 && one.data && two.data && !strcmp(one.data, "hello from Veda\n") &&
	      !strcmp(two.data, TEXT), "%d %s", status, err);
	empty(&one);
	empty(&two);
	char config[512];
	int len = snprintf(config, sizeof config, "url = \"%s/hello\"\noutput = \"configured.txt\"\nsilent\n", base);
	write_file("config.txt", config, (size_t)len);
	status = run_curl("-K", "config.txt");
	file = read_file("configured.txt");
	CHECK("curl -K", status == 0 && file.data && !strcmp(file.data, "hello from Veda\n"), "%d %s", status, err);
	empty(&file);

	/* A proxy, from the environment. */
	char proxy[96], *env[64];
	int e = 0;
	snprintf(proxy, sizeof proxy, "http_proxy=%s", base);
	for (char **v = environ; *v && e < 62; v++)
		if (strncasecmp(*v, "http_proxy=", 11) && strncasecmp(*v, "no_proxy=", 9))
			env[e++] = *v;
	env[e++] = proxy;
	env[e] = 0;
	status = run_curl_in(env, (const char *const[]){ "-sS", "http://far.example/headers", 0 });
	CHECK("curl through $http_proxy", status == 0 && strstr(out.data, "GET http://far.example/headers HTTP/1.1"),
	      "%d, '%s' %s", status, out.data, err);

	/* Failures, as their exit statuses say. */
	snprintf(url, sizeof url, "http://127.0.0.1:%d/", closed_port());
	status = run_curl("-sS", url);
	CHECK("curl of a port nobody listens on", status == 7, "%d %s", status, err);
	status = run_curl("-sS", "http://nonexistent.invalid/");
	CHECK("curl of a name nothing has", status == 6, "%d %s", status, err);
	snprintf(url, sizeof url, "%s/slow", base);
	status = run_curl("-sS", "-m", "1", url);
	CHECK("curl -m 1, of a server that waits", status == 28, "%d %s", status, err);

	/* SIGINT (Ctrl+C) ends it as it waits, once the server has its
	   request (within half a minute, where the processor is emulated). */
	int seen = atomic_load(&stalled);
	pid_t pid = start_curl((const char *const[]){ "-sS", url, 0 }, environ);
	for (int i = 0; i < 3000 && atomic_load(&stalled) == seen; i++)
		usleep(10000);
	usleep(100000);
	if (pid > 0)
		kill(pid, SIGINT);
	status = finish(pid);
	CHECK("curl ended by SIGINT", status == 128 + SIGINT, "%d", status);

	/* Its manual, built in. */
	status = run_curl("--manual");
	CHECK("curl --manual", status == 0 && strstr(out.data, "curl - transfer a URL") && out.len > 100000,
	      "%d, %zu bytes %s", status, out.len, err);

	if (host) {
		snprintf(url, sizeof url, "http://%s:%d/hello", host, host_port);
		status = run_curl("-sS", "--retry", "20", "--retry-connrefused", "--retry-delay", "1", url);
		CHECK("curl of the host's web server", status == 0 && !strcmp(out.data, "hello from the host\n"),
		      "%d, '%s' %s", status, out.data, err);
	}
	empty(&out);
	empty(&errors);
}

int main(int argc, char **argv)
{
	setvbuf(stdout, 0, _IOLBF, 0);
	/* Names in the locale's encoding, which is UTF-8, as the curl
	   program's are (it sets the locale): IDN takes them so. */
	setlocale(LC_ALL, "");
	/* A server writes to clients that may have gone. */
	signal(SIGPIPE, SIG_IGN);
	const char *host = argc >= 5 && !strcmp(argv[1], "host") ? argv[2] : 0;
	int host_port = host ? atoi(argv[3]) : 0;
	CHECK("curl_global_init", curl_global_init(CURL_GLOBAL_DEFAULT) == CURLE_OK, "failed");
	if (!start_servers()) {
		printf("FAIL: %d failed (the servers did not start)\n", failures + 1);
		return failures + 1;
	}
	snprintf(base, sizeof base, "http://127.0.0.1:%d", port);
	snprintf(base6, sizeof base6, "http://[::1]:%d", port);
	snprintf(named, sizeof named, "http://localhost:%d", port);
	snprintf(secure, sizeof secure, "https://localhost:%d", port);
	snprintf(mutual, sizeof mutual, "https://localhost:%d", mutual_port);
	check_version();
	check_authorities();
	check_http();
	check_uploads();
	check_compression();
	check_downloads();
	check_proxy();
	check_tls();
	check_multi();
	check_threads();
	check_http2();
	check_cookies();
	check_hsts();
	check_websockets();
	check_files();
	check_urls();
	if (host)
		check_host(host, host_port);
	check_program(host, host_port);
	curl_global_cleanup();
	printf("%s: %d failed\n", failures ? "FAIL" : "PASS", failures);
	return failures;
}
