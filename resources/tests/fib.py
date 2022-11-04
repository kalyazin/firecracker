# Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0

"""Calculate Fibonacci sequence using recursion"""

from sys import argv

def usage():
    print("Usage: " + argv[0] + " <pos_num>")

def fib_rec(n):
   if n <= 1:
       return n
   else:
       return fib_rec(n - 1) + fib_rec(n - 2)

if (len(argv) != 2):
    usage()
    exit(1)

n = int(argv[1])
if n <= 0:
   usage()
   exit(1)
else:
   for i in range(n):
       print(fib_rec(i))
