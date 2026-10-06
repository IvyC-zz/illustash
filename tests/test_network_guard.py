"""The suite guard rejects external destinations without opening a connection."""
import unittest
import urllib.request
from unittest import mock

from tests import network_guard


class NetworkGuard(unittest.TestCase):
    def test_only_loopback_http_is_allowed(self):
        for url in ('http://127.0.0.1:8123/test', 'http://127.0.0.2/test',
                    'http://[::1]:8123/test', 'https://localhost:8123/test'):
            with self.subTest(url=url):
                self.assertTrue(network_guard.is_local_http(url))
        for url in ('https://danbooru.donmai.us/wiki_pages.json',
                    'http://localhost.example/test', 'http://192.168.1.1/test',
                    'http://0.0.0.0/test', 'file:///tmp/test',
                    'http://localhost@external.example/test'):
            with self.subTest(url=url):
                self.assertFalse(network_guard.is_local_http(url))

    def test_guard_blocks_strings_and_requests_before_transport(self):
        with mock.patch.object(network_guard, '_open') as transport, \
                mock.patch.object(network_guard, 'blocked_requests', []):
            for value in ('https://external.example/test',
                          urllib.request.Request('https://external.example/test')):
                with self.assertRaises(AssertionError):
                    urllib.request.build_opener().open(value)
            transport.assert_not_called()
            self.assertEqual(len(network_guard.blocked_requests), 2)

    def test_local_requests_keep_transport_arguments(self):
        request = urllib.request.Request('http://127.0.0.1:8123/test')
        opener = urllib.request.build_opener()
        with mock.patch.object(network_guard, '_open', return_value='response') as transport, \
                mock.patch.object(network_guard, 'local_requests', 0):
            self.assertEqual(opener.open(request, timeout=3), 'response')
        transport.assert_called_once_with(opener, request, timeout=3)
