return {
    tools = {
        {
            name = "echo_tool",
            description = "Echoes input value back",
            schema = {
                type = "object",
                properties = {
                    msg = { type = "string" }
                }
            },
            handler = function(input)
                if input.trigger_toast then
                    pacode.toast("hello from echo_tool")
                    pacode.status("running echo_tool")
                end
                return input
            end
        },
        {
            name = "allocate_mem",
            description = "Allocates memory exceeding limit",
            schema = {},
            handler = function(_)
                local chunks = {}
                for i = 1, 1000000 do
                    chunks[i] = string.rep("A", 1024 * 1024)
                end
                return { count = #chunks }
            end
        },
        {
            name = "infinite_loop",
            description = "Runs forever to test timeout",
            schema = {},
            handler = function(_)
                while true do
                end
            end
        }
    },
    commands = {
        {
            name = "echo_cmd",
            description = "Echoes args as insert text",
            handler = function(args)
                return { insert_text = "inserted: " .. args }
            end
        },
        {
            name = "prompt_cmd",
            description = "Sends args as prompt",
            handler = function(args)
                return { send_prompt = "sent: " .. args }
            end
        },
        {
            name = "toast_cmd",
            description = "Triggers toast and returns nothing",
            handler = function(args)
                pacode.toast("toast: " .. args)
                pacode.status("status: " .. args)
                return "nothing"
            end
        }
    },
    hooks = {
        pre_tool_call = function(name, input)
            if input and input.deny then
                return { deny = "denied by pre_tool_call hook" }
            end
            if input and input.modify then
                return {
                    modify_input = {
                        modified = true,
                        injected = "extra_field",
                        orig = input
                    }
                }
            end
            if input and input.toast_pre then
                pacode.toast("toast from pre_tool_call")
            end
            return nil
        end,
        post_tool_call = function(name, input, output)
            return nil
        end,
        turn_start = function()
            pacode.status("turn started")
            return nil
        end,
        turn_end = function(stats)
            pacode.toast("turn ended in " .. tostring(stats.duration_ms) .. "ms")
            return nil
        end,
        on_message = function(role, text)
            return nil
        end
    }
}
