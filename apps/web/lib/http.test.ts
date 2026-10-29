import { expect, test } from "bun:test";
import { ipKey } from "./http";

test("ipKey: IPv4 as is, IPv6 by /64, IPv4-mapped as IPv4", () => {
  expect(ipKey("203.0.113.7")).toBe("203.0.113.7");
  expect(ipKey("2001:db8:a:b:1:2:3:4")).toBe("2001:db8:a:b::/64");
  expect(ipKey("2001:db8:a:b::99")).toBe("2001:db8:a:b::/64");
  expect(ipKey("2001:DB8:0a::1")).toBe("2001:db8:a:0::/64");
  expect(ipKey("::1")).toBe("0:0:0:0::/64");
  expect(ipKey("::ffff:198.51.100.1")).toBe("198.51.100.1");
  expect(ipKey("unknown")).toBe("unknown");
});
