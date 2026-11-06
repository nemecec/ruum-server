#!/usr/bin/env python3
"""Create, show or delete the Ruum build server at Hetzner (design 18: agents build there).

    python3 scripts/buildbox/hcloud.py create [--type cax41] [--location hel1] --allow-ip 1.2.3.4
    python3 scripts/buildbox/hcloud.py status
    python3 scripts/buildbox/hcloud.py delete

Reads HCLOUD_TOKEN from ~/.config/ruum/secrets.env and the SSH key from
~/.config/ruum/buildbox_ed25519.pub. It never prints secrets. The server costs money while it
exists (cax41 in hel1: about EUR 0.08 per hour), so delete it when no run needs it.
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

API = 'https://api.hetzner.cloud/v1'
NAME = 'ruum-buildbox'
LABELS = {'purpose': 'buildbox', 'project': 'ruum'}
HERE = os.path.dirname(os.path.abspath(__file__))


def token():
    for line in open(os.path.expanduser('~/.config/ruum/secrets.env')):
        if line.startswith('HCLOUD_TOKEN='):
            return line.split('=', 1)[1].strip().strip('"').strip("'")
    sys.exit('HCLOUD_TOKEN is missing in ~/.config/ruum/secrets.env')


def call(method, path, body=None):
    req = urllib.request.Request(API + path, method=method,
                                 data=json.dumps(body).encode() if body is not None else None)
    req.add_header('Authorization', 'Bearer ' + token())
    req.add_header('Content-Type', 'application/json')
    try:
        with urllib.request.urlopen(req) as r:
            raw = r.read()
            return r.status, json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, json.loads(raw) if raw else {}


def find(kind, name):
    st, d = call('GET', f'/{kind}?name={name}')
    items = d.get(kind, [])
    return items[0] if items else None


def ensure_ssh_key():
    pub = open(os.path.expanduser('~/.config/ruum/buildbox_ed25519.pub')).read().strip()
    key = find('ssh_keys', NAME)
    if key:
        return key['id']
    st, d = call('POST', '/ssh_keys', {'name': NAME, 'public_key': pub, 'labels': LABELS})
    if st >= 300:
        sys.exit(f'ssh key: HTTP {st} {d.get("error")}')
    return d['ssh_key']['id']


def ensure_firewall(allow_ip):
    rules = [{'direction': 'in', 'protocol': 'tcp', 'port': '22', 'source_ips': [f'{allow_ip}/32']},
             {'direction': 'in', 'protocol': 'icmp', 'source_ips': [f'{allow_ip}/32']}]
    fw = find('firewalls', NAME)
    if fw:
        call('POST', f'/firewalls/{fw["id"]}/actions/set_rules', {'rules': rules})
        return fw['id']
    st, d = call('POST', '/firewalls', {'name': NAME, 'labels': LABELS, 'rules': rules})
    if st >= 300:
        sys.exit(f'firewall: HTTP {st} {d.get("error")}')
    return d['firewall']['id']


def create(args):
    if find('servers', NAME):
        print('already exists'); return status(args)
    key_id = ensure_ssh_key()
    fw_id = ensure_firewall(args.allow_ip)
    user_data = open(os.path.join(HERE, 'cloud-init.yaml')).read()
    st, d = call('POST', '/servers', {
        'name': NAME, 'server_type': args.type, 'location': args.location, 'image': 'ubuntu-24.04',
        'ssh_keys': [key_id], 'firewalls': [{'firewall': fw_id}], 'labels': LABELS,
        'user_data': user_data, 'public_net': {'enable_ipv4': True, 'enable_ipv6': True},
    })
    if st >= 300:
        sys.exit(f'server: HTTP {st} {d.get("error")}')
    sid = d['server']['id']
    for _ in range(60):
        st, d = call('GET', f'/servers/{sid}')
        if d['server']['status'] == 'running':
            break
        time.sleep(5)
    status(args)


def status(args):
    s = find('servers', NAME)
    if not s:
        print('no build server'); return
    t = s['server_type']
    print(f"{s['name']} {s['status']} {t['name']} ({t['cores']} vCPU, {t['memory']} GB) "
          f"ip {s['public_net']['ipv4']['ip']} created {s['created']}")


def delete(args):
    s = find('servers', NAME)
    if not s:
        print('no build server'); return
    st, d = call('DELETE', f"/servers/{s['id']}")
    print(f'delete server: HTTP {st}')


p = argparse.ArgumentParser()
p.add_argument('cmd', choices=['create', 'status', 'delete'])
p.add_argument('--type', default='cax41')
p.add_argument('--location', default='hel1')
p.add_argument('--allow-ip', default=None)
a = p.parse_args()
if a.cmd == 'create' and not a.allow_ip:
    sys.exit('create needs --allow-ip (the public IP that may use SSH)')
{'create': create, 'status': status, 'delete': delete}[a.cmd](a)
