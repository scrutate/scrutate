-- Lua sample
local util = require("demo.util")
local M = {}
-- Greets.
function M.greet(name)
  return "hi " .. name
end
local function helper(x)
  return M.greet(x)
end
function Account:deposit(v) self.balance = self.balance + v end
return M
