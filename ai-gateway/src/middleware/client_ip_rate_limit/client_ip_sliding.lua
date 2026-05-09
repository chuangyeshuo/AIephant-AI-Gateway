-- KEYS[1]: ZSET key per client IP
-- ARGV[1]: window length in milliseconds (1000)
-- ARGV[2]: max requests in window
-- ARGV[3]: unique member for this request
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
local window = tonumber(ARGV[1])
local lim = tonumber(ARGV[2])
local member = ARGV[3]
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now - window)
local c = redis.call('ZCARD', KEYS[1])
if c >= lim then
  return 0
end
redis.call('ZADD', KEYS[1], now, member)
redis.call('PEXPIRE', KEYS[1], window + 1000)
return 1
