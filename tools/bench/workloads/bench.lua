-- Lua workloads for tools/bench/suite.mjs: each prints its name, a checksum
-- (the same on every tier) and the CPU time it took.
--   lua bench.lua [scale]
local scale = tonumber(arg and arg[1]) or 1

local function run(name, f)
  local t0 = os.clock()
  local sum = f()
  print(string.format("%-12s %12s %8.3f", name, tostring(sum), os.clock() - t0))
end

-- Recursive calls.
run("fib", function()
  local function fib(n) if n < 2 then return n end return fib(n - 1) + fib(n - 2) end
  return fib(27 + math.floor(math.log(scale, 2)))
end)

-- Tables as hash maps and arrays.
run("tables", function()
  local t, s = {}, 0
  for i = 1, 200000 * scale do t["k" .. (i % 5000)] = (t["k" .. (i % 5000)] or 0) + i end
  for _, v in pairs(t) do s = (s + v) % 1000000007 end
  local a = {}
  for i = 1, 300000 * scale do a[i] = i * 3 % 1000 end
  for i = 1, #a do s = (s + a[i]) % 1000000007 end
  return s
end)

-- Strings: concatenation, formatting, patterns.
run("strings", function()
  local parts = {}
  for i = 1, 30000 * scale do parts[#parts + 1] = string.format("%d:%x;", i, i * 7) end
  local s = table.concat(parts)
  local n = 0
  for a, b in s:gmatch("(%d+):(%x+);") do n = (n + #a + #b) % 1000000007 end
  local r = s:gsub("%d", "#")
  return n + #r
end)

-- Sorting with a comparator closure.
run("sort", function()
  local a, x = {}, 12345
  for i = 1, 100000 * scale do x = (x * 1103515245 + 12345) % 2147483648; a[i] = x end
  table.sort(a, function(p, q) return p > q end)
  local s = 0
  for i = 1, #a, 97 do s = (s + a[i]) % 1000000007 end
  return s
end)

-- Objects: metatables and method calls.
run("objects", function()
  local Point = {}
  Point.__index = Point
  function Point.new(x, y) return setmetatable({ x = x, y = y }, Point) end
  function Point:add(o) return Point.new(self.x + o.x, self.y + o.y) end
  function Point:len2() return self.x * self.x + self.y * self.y end
  local p, s = Point.new(0, 0), 0
  for i = 1, 300000 * scale do
    p = p:add(Point.new(i % 7, -(i % 5)))
    s = (s + p:len2()) % 1000000007
  end
  return s
end)

-- Floating point: n-body style arithmetic.
run("float", function()
  local x, y, vx, vy = 1.0, 0.0, 0.0, 1.0
  local dt = 0.001
  for _ = 1, 1000000 * scale do
    local r2 = x * x + y * y
    local r3 = r2 * math.sqrt(r2)
    vx, vy = vx - x / r3 * dt, vy - y / r3 * dt
    x, y = x + vx * dt, y + vy * dt
  end
  return string.format("%.6f", x + y)
end)
