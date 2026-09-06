#include <stdalign.h>
_Thread_local alignas(64) __attribute__((used, visibility("default"))) char herdr_tls_align_fix[64];
__attribute__((used))
void *herdr_tls_keepalive(void) { return herdr_tls_align_fix; }
