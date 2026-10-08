/* The name lookups games make before they play locally: the machine's
 * name, and that it and localhost resolve (localhost to the loopback
 * address). Prints only what holds on any machine, with or without a
 * network, so the output is the same natively.
 *
 * libs: -lws2_32
 */
#include <winsock2.h>
#include <stdio.h>

int main(void)
{
    WSADATA wsa;
    char host[256] = "";
    struct hostent *he;

    printf("WSAStartup: %d\n", WSAStartup(MAKEWORD(2, 2), &wsa));
    printf("gethostname: %d\n", gethostname(host, sizeof(host)));
    printf("host name is printable: %d\n", host[0] > ' ' && host[0] < 127);
    printf("gethostname into a 1-byte buffer fails: %d\n", gethostname(host + 200, 1) == SOCKET_ERROR);
    he = gethostbyname(host);
    printf("the machine's name resolves: %d\n", he && he->h_addrtype == AF_INET && he->h_length == 4 && he->h_addr_list[0]);
    he = gethostbyname("localhost");
    printf("localhost resolves: %d\n", he != NULL);
    printf("localhost is 127.0.0.1: %d\n", he && !memcmp(he->h_addr_list[0], "\x7f\0\0\1", 4));
    WSACleanup();
    return 0;
}
