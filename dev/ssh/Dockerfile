# Throwaway SSH bastion for tunnel tests (`make test-live`): user/password tangul/tangul,
# TCP forwarding on
# (a drop-in, because sshd takes the first value seen and the stock config sets it to no). Reaches the other compose services by name (postgres, mysql).
FROM alpine:3.20
RUN apk add --no-cache openssh \
 && ssh-keygen -A \
 && adduser -D -s /bin/sh tangul \
 && echo 'tangul:tangul' | chpasswd \
 && printf 'PasswordAuthentication yes\nAllowTcpForwarding yes\nPermitRootLogin no\n' > /etc/ssh/sshd_config.d/bastion.conf
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
