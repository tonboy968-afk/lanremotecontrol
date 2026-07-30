#!/usr/bin/env python3
"""Test client: perform handshake + receive first frame from host."""
import socket
import struct
import time
import sys

HOST = "127.0.0.1"
PORT = 50000

# bincode serializes:
# - enum (#[repr(u8)] but bincode uses u32 LE by default): 4 bytes LE
# - u32: 4 bytes LE
# - u16: 2 bytes LE
# - String/Vec<u8>: u64 LE length + bytes
# - struct: fields in order, no padding

# Message struct fields:
#   message_type: MessageType (enum 鈫?u32 LE)
#   sequence_number: u32
#   payload_length: u32
#   reserved: u16
#   payload: Vec<u8> (u64 LE len + bytes)

def make_message(msg_type_u32, seq, payload):
    """Build a bincode-serialized Message."""
    header = struct.pack('<IIIH', msg_type_u32, seq, len(payload), 0)
    vec_prefix = struct.pack('<Q', len(payload))
    return header + vec_prefix + payload

def parse_message(data):
    """Parse a Message from bytes (bincode format)."""
    if len(data) < 14:
        return None
    msg_type, seq, plen, reserved = struct.unpack('<IIIH', data[:14])
    vec_len = struct.unpack('<Q', data[14:22])[0]
    payload = data[22:22+vec_len]
    return (msg_type, seq, payload)

# bincode serializes fieldless enums as variant INDEX (0-based), NOT discriminant value
# MessageType variants in order:
#   0 = ControlCommand (0x01)
#   1 = ScreenFrame (0x02)
#   2 = Ack (0x03)
#   3 = Heartbeat (0x04)
#   4 = ConnectionManagement (0x05)
#   5 = ScreenFrameChunk (0x06)
#   6 = ScreenFrameChunkDelta (0x07)
MSG_CONTROL = 0
MSG_SCREENFRAME = 1
MSG_ACK = 2
MSG_HEARTBEAT = 3
MSG_CONNMGMT = 4
MSG_FRAMECHUNK = 5
MSG_FRAMECHUNK_DELTA = 6

# ConnectionManagementPayload enum variants:
# Request = 0, Capabilities = 1, Confirm = 2, Teardown = 3

# ConnectionRequest { auth_token: String, protocol_version: u32 }
# bincode String = u64 LE length + UTF-8 bytes
def make_conn_request():
    variant_idx = struct.pack('<I', 0)  # Request = 0
    auth_token = b''
    auth_token_encoded = struct.pack('<Q', len(auth_token)) + auth_token
    protocol_version = struct.pack('<I', 1)
    return variant_idx + auth_token_encoded + protocol_version

# ConnectionConfirm { chosen_encoding: String }
def make_conn_confirm():
    variant_idx = struct.pack('<I', 2)  # Confirm = 2
    encoding = b'lz4'
    encoding_encoded = struct.pack('<Q', len(encoding)) + encoding
    return variant_idx + encoding_encoded

def make_ack(seq):
    """Ack payload is just u32 LE seq number, but wrapped in Vec<u8>"""
    # Actually, the payload field is Vec<u8>, and create_ack puts seq.to_le_bytes() into it
    # So payload = u64 LE len(4) + 4 bytes of seq
    ack_data = struct.pack('<I', seq)
    return ack_data  # This goes as the payload bytes in the Message

def main():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(3.0)
    sock.connect((HOST, PORT))
    print(f"[*] Connected to {HOST}:{PORT}")

    # Step 1: Send ConnectionRequest
    req_payload = make_conn_request()
    msg = make_message(MSG_CONNMGMT, 1, req_payload)
    sock.send(msg)
    print(f"[鈫抅 Sent ConnectionRequest ({len(msg)} bytes): {msg.hex()}")

    # Step 2: Receive CapabilitiesResponse
    try:
        data, _ = sock.recvfrom(65536)
        print(f"[鈫怾 Received {len(data)} bytes")
        parsed = parse_message(data)
        if parsed is None:
            print("[!] Failed to parse message")
            return
        msg_type, seq, payload = parsed
        print(f"    type=0x{msg_type:02x}, seq={seq}, payload_len={len(payload)}")
        
        if msg_type != MSG_CONNMGMT:
            print(f"[!] Expected ConnectionManagement (0x05), got 0x{msg_type:02x}")
            return
        
        # Parse CapabilitiesResponse (variant 1)
        if len(payload) < 4:
            print("[!] Payload too short")
            return
        variant = struct.unpack('<I', payload[:4])[0]
        print(f"    variant={variant}")
        if variant == 1:
            print("[鉁揮 Got CapabilitiesResponse")
        else:
            print(f"[!] Expected variant 1 (Capabilities), got {variant}")
            return
    except socket.timeout:
        print("[!] Timeout waiting for CapabilitiesResponse")
        return

    # Step 3: Send ConnectionConfirm
    confirm_payload = make_conn_confirm()
    msg = make_message(MSG_CONNMGMT, 2, confirm_payload)
    sock.send(msg)
    print(f"[鈫抅 Sent ConnectionConfirm ({len(msg)} bytes)")

    # Step 4: Receive frames
    print("[*] Waiting for screen frames...")
    frame_count = 0
    chunk_count = 0
    msg_types = {}
    assemblies = {}
    
    sock.settimeout(5.0)
    while frame_count < 3:
        try:
            data, _ = sock.recvfrom(65536)
        except socket.timeout:
            print(f"[!] Timeout. Received {chunk_count} chunks, {frame_count} complete frames.")
            break
        
        parsed = parse_message(data)
        if parsed is None:
            continue
        msg_type, seq, payload = parsed
        
        msg_types[msg_type] = msg_types.get(msg_type, 0) + 1
        
        if msg_type in (MSG_FRAMECHUNK, MSG_FRAMECHUNK_DELTA):
            chunk_count += 1
            frame_type = "delta" if msg_type == MSG_FRAMECHUNK_DELTA else "full"
            
            # Parse ScreenFrameChunk (bincode)
            # msg_id:u32, chunk_count:u32, chunk_idx:u32, total_data_len:u32, width:u32, height:u32, data:Vec<u8>
            if len(payload) < 28:
                continue
            f_msg_id, f_chunk_count, f_chunk_idx, f_total_len, f_width, f_height = struct.unpack('<IIIIII', payload[:24])
            data_len = struct.unpack('<Q', payload[24:32])[0]
            chunk_data = payload[32:32+data_len]
            
            if f_chunk_idx == 0:
                print(f"  [chunk 0/{f_chunk_count}] msg_id={f_msg_id} {f_width}x{f_height} type={frame_type} total={f_total_len}")
            
            key = f_msg_id
            if key not in assemblies:
                assemblies[key] = {'chunks': {}, 'count': f_chunk_count, 'width': f_width, 'height': f_height, 'type': frame_type}
            assemblies[key]['chunks'][f_chunk_idx] = chunk_data
            
            if len(assemblies[key]['chunks']) == assemblies[key]['count']:
                full_data = b''
                for i in range(assemblies[key]['count']):
                    full_data += assemblies[key]['chunks'][i]
                w = assemblies[key]['width']
                h = assemblies[key]['height']
                ft = assemblies[key]['type']
                frame_count += 1
                print(f"[馃摲] Frame #{frame_count}: {w}x{h} ({len(full_data)} bytes, type={ft})")
                del assemblies[key]
        
        elif msg_type == MSG_HEARTBEAT:
            ack_payload = struct.pack('<I', seq)
            ack_msg = make_message(MSG_ACK, 0, ack_payload)
            sock.send(ack_msg)
        
        elif msg_type == MSG_ACK:
            pass
    
    print(f"\n[*] Summary: {chunk_count} chunks, {frame_count} frames")
    print(f"[*] Message types: {dict((hex(k), v) for k, v in msg_types.items())}")
    
    if frame_count == 0:
        print("[!] NO FRAMES RECEIVED 鈥?host may not be sending")
    elif frame_count > 0:
        print("[鉁揮 Frames received successfully!")
    
    sock.close()

if __name__ == '__main__':
    main()


