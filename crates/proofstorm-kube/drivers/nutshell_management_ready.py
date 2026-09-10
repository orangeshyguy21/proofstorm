"""Bounded authenticated management + HTTP readiness, without CLI error parsing."""
import pathlib
import sys
import urllib.request

import grpc
from cashu.mint.management_rpc.protos import management_pb2, management_pb2_grpc

tls = pathlib.Path('/management-client/tls')
credentials = grpc.ssl_channel_credentials(
    root_certificates=(tls / 'ca.pem').read_bytes(),
    private_key=(tls / 'client.key').read_bytes(),
    certificate_chain=(tls / 'client.pem').read_bytes(),
)
with grpc.secure_channel('127.0.0.1:8086', credentials) as channel:
    management_pb2_grpc.MintStub(channel).GetInfo(management_pb2.GetInfoRequest(), timeout=1)
urllib.request.urlopen(sys.argv[1], timeout=1).read()
