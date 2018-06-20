#ifndef _ALLOC_H
#define _ALLOC_H

#include <stddef.h>

void halloc_init(size_t base, size_t size);
void *halloc(size_t size);
void hfree(void *ptr);
void *halloc_aligned(size_t size, size_t align);

#endif  /* _ALLOC_H */
