import unittest
import test.test_urllib as tu
# 直接跑这两个失败测试
suite = unittest.TestSuite()
suite.addTest(tu.urlopen_HttpTests('test_url_host_with_control_char_rejected'))
suite.addTest(tu.urlopen_HttpTests('test_url_host_with_newline_header_injection_rejected'))
r = unittest.TextTestRunner(verbosity=2).run(suite)
