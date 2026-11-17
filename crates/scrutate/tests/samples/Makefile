# Make sample
CC = gcc
CFLAGS = -O2

# Builds everything.
all: main.o util.o
	$(CC) -o app main.o util.o
	$(call banner)

clean:
	rm -f *.o app

define banner
	echo hi
endef
