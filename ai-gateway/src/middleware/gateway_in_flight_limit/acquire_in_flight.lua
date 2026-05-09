-- KEYS[1] = counter key
-- ARGV[1] = TTL seconds
-- ARGV[2] = max concurrent (inclusive)
local n = redis.call('INCR', KEYS[1])
redis.call('EXPIRE', KEYS[1], ARGV[1])
local max = tonumber(ARGV[2])
if n > max then
  redis.call('DECR', KEYS[1])
  return 0
end
return 1
