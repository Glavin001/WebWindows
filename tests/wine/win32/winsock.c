/* What games do to play locally, with themselves as the server: the
 * machine's name, that it and localhost resolve (localhost to the loopback
 * address), and UDP datagrams between two sockets over loopback (bound to a
 * port the system picks, with select, non-blocking reads and a buffer too
 * small). Prints only what holds on any machine, with or without a
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

    {
        SOCKET a = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP), b = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
        struct sockaddr_in at = {0}, from = {0};
        int len = sizeof(at), r, on = 1;
        u_long nb = 1;
        char buf[16];
        fd_set fds;
        struct timeval tv = {2, 0};

        printf("UDP sockets: %d\n", a != INVALID_SOCKET && b != INVALID_SOCKET);
        at.sin_family = AF_INET;
        at.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        printf("bind to a port the system picks: %d\n", bind(a, (struct sockaddr *)&at, sizeof(at)));
        printf("getsockname gives the port: %d\n", !getsockname(a, (struct sockaddr *)&at, &len) && at.sin_port != 0);
        printf("SO_BROADCAST: %d\n", setsockopt(b, SOL_SOCKET, SO_BROADCAST, (char *)&on, sizeof(on)));
        printf("sendto over loopback: %d\n", sendto(b, "datagram", 8, 0, (struct sockaddr *)&at, sizeof(at)));
        FD_ZERO(&fds);
        FD_SET(a, &fds);
        printf("select sees it: %d\n", select(0, &fds, NULL, NULL, &tv));
        len = sizeof(from);
        r = recvfrom(a, buf, sizeof(buf), 0, (struct sockaddr *)&from, &len);
        printf("recvfrom: %d '%.*s' from loopback: %d\n", r, r > 0 ? r : 0, buf, from.sin_addr.s_addr == htonl(INADDR_LOOPBACK));
        printf("FIONBIO: %d\n", ioctlsocket(a, FIONBIO, &nb));
        r = recv(a, buf, sizeof(buf), 0);
        printf("non-blocking recv with nothing queued: %d, WSAEWOULDBLOCK: %d\n", r, WSAGetLastError() == WSAEWOULDBLOCK);
        sendto(b, "0123456789", 10, 0, (struct sockaddr *)&at, sizeof(at));
        r = recv(a, buf, 4, 0);
        printf("into a buffer too small: %d, WSAEMSGSIZE: %d\n", r, WSAGetLastError() == WSAEMSGSIZE);
        closesocket(a);
        closesocket(b);
    }
    WSACleanup();
    return 0;
}
