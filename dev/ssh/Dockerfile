# Throwaway SSH bastion for tunnel tests (`make test-live`): user/password rusdb/rusdb,
# TCP forwarding on. Reaches the other compose services by name (postgres, mysql).
FROM alpine:3.20
RUN apk add --no-cache openssh \
 && ssh-keygen -A \
 && adduser -D -s /bin/sh rusdb \
 && echo 'rusdb:rusdb' | chpasswd \
 && printf 'PasswordAuthentication yes\nAllowTcpForwarding yes\nPermitRootLogin no\n' >> /etc/ssh/sshd_config
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
