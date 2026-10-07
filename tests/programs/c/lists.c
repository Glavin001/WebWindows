/* Heap allocation, linked structures, qsort with a callback. */
#include <stdio.h>
#include <stdlib.h>

struct node { int key; struct node *left, *right; };

static struct node *insert(struct node *t, int key) {
    if (!t) { t = calloc(1, sizeof *t); t->key = key; return t; }
    if (key < t->key) t->left = insert(t->left, key); else t->right = insert(t->right, key);
    return t;
}
static int depth(struct node *t) { if (!t) return 0; int l = depth(t->left), r = depth(t->right); return 1 + (l > r ? l : r); }
static long sum(struct node *t) { return t ? t->key + sum(t->left) + sum(t->right) : 0; }
static void destroy(struct node *t) { if (t) { destroy(t->left); destroy(t->right); free(t); } }
static int cmp(const void *a, const void *b) { int x = *(const int *)a, y = *(const int *)b; return (x > y) - (x < y); }

int main(void) {
    struct node *t = 0;
    unsigned s = 12345;
    int arr[200];
    for (int i = 0; i < 200; i++) { s = s * 1103515245 + 12345; int k = (s >> 8) % 1000; t = insert(t, k); arr[i] = k; }
    printf("depth %d sum %ld\n", depth(t), sum(t));
    destroy(t);
    qsort(arr, 200, sizeof arr[0], cmp);
    printf("sorted %d %d %d %d\n", arr[0], arr[1], arr[100], arr[199]);
    int *v = 0; int n = 0;
    for (int i = 0; i < 100; i++) { v = realloc(v, (n + 1) * sizeof *v); v[n++] = i * i; }
    printf("realloc %d %d\n", v[50], v[99]);
    free(v);
    return 0;
}
