local plugin = {
    tools = {
        {
            name = "echo_tool",
            description = "Echo tool",
            schema = {
                type = "object",
                properties = {
                    msg = { type = "string" }
                }
            },
            handler = function(input)
                return { result = input.msg or "default" }
            end
        }
    },
    commands = {
        {
            name = "echo_cmd",
            description = "Echo command",
            handler = function(args)
                return { insert_text = "echo: " .. (args or "") }
            end
        }
    },
    hooks = {
        pre_tool_call = function(name, input)
            if name == "blocked_tool" or (type(input) == "table" and input.block == true) then
                return { deny = "blocked by test hook" }
            end
            return { continue = true }
        end
    }
}

return plugin
