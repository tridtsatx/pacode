#!/usr/bin/env python3
import json
import sys
import time

def handle_request(req):
    method = req.get("method")
    req_id = req.get("id")
    params = req.get("params") or {}

    if method == "initialize":
        caps = {"tools": {}}
        import os
        if os.environ.get("FAKE_MCP_RESOURCES") == "1":
            caps["resources"] = {}
        if os.environ.get("FAKE_MCP_PROMPTS") == "1":
            caps["prompts"] = {}
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": caps,
                "serverInfo": {"name": "fake_mcp", "version": "0.1.0"},
            },
        }

    import os
    if method == "resources/list" and os.environ.get("FAKE_MCP_RESOURCES") == "1":
        cursor = params.get("cursor")
        if not cursor:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "resources": [
                        {
                            "uri": "fake://resource1",
                            "name": "Resource 1",
                            "description": "First test resource",
                            "mimeType": "text/plain",
                        }
                    ],
                    "nextCursor": "res_page_2",
                },
            }
        elif cursor == "res_page_2":
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "resources": [
                        {
                            "uri": "fake://resource2",
                            "name": "Resource 2",
                            "description": "Second test resource",
                            "mimeType": "image/png",
                        }
                    ],
                },
            }
        else:
            return {"jsonrpc": "2.0", "id": req_id, "result": {"resources": []}}

    if method == "resources/read" and os.environ.get("FAKE_MCP_RESOURCES") == "1":
        uri = params.get("uri")
        if uri == "fake://resource1":
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "contents": [
                        {
                            "uri": "fake://resource1",
                            "mimeType": "text/plain",
                            "text": "content of resource1",
                        }
                    ]
                },
            }
        else:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "contents": [
                        {
                            "uri": uri,
                            "mimeType": "image/png",
                            "blob": "aW1hZ2VkYXRh",
                        }
                    ]
                },
            }

    if method == "prompts/list" and os.environ.get("FAKE_MCP_PROMPTS") == "1":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "prompts": [
                    {
                        "name": "test_prompt",
                        "description": "A test prompt",
                        "arguments": [
                            {
                                "name": "topic",
                                "description": "The prompt topic",
                                "required": True,
                            }
                        ],
                    }
                ]
            },
        }

    if method == "prompts/get" and os.environ.get("FAKE_MCP_PROMPTS") == "1":
        name = params.get("name")
        args = params.get("arguments") or {}
        topic = args.get("topic", "default")
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "description": "Rendered test prompt",
                "messages": [
                    {
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": f"Tell me about {topic}",
                        },
                    }
                ],
            },
        }

    if method == "tools/list":
        cursor = params.get("cursor")
        if not cursor:
            # Page 1
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echoes arguments",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "text": {"type": "string"},
                                },
                            },
                        },
                    ],
                    "nextCursor": "page_2",
                },
            }
        elif cursor == "page_2":
            # Page 2
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "tools": [
                        {
                            "name": "fail",
                            "description": "Fails tool call",
                            "inputSchema": {"type": "object"},
                        },
                        {
                            "name": "slow",
                            "description": "Sleeps 3 seconds",
                            "inputSchema": {"type": "object"},
                        },
                    ],
                },
            }
        else:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "tools": [],
                },
            }

    if method == "tools/call":
        tool_name = params.get("name")
        args = params.get("arguments") or {}

        if tool_name == "echo":
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": json.dumps(args, sort_keys=True),
                        }
                    ],
                    "isError": False,
                },
            }
        elif tool_name == "test_sampling":
            # Send sampling/createMessage request to client
            sample_req = {
                "jsonrpc": "2.0",
                "id": 8888,
                "method": "sampling/createMessage",
                "params": {
                    "messages": [
                        {
                            "role": "user",
                            "content": {
                                "type": "text",
                                "text": "hello AI from fake_mcp",
                            },
                        }
                    ],
                    "maxTokens": 4096,
                },
            }
            sys.stdout.write(json.dumps(sample_req) + "\n")
            sys.stdout.flush()
            reply_line = sys.stdin.readline()
            try:
                reply = json.loads(reply_line)
                res = reply.get("result")
                content = res.get("content", {})
                text = content.get("text", "")
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": f"sampling response received: {text}",
                            }
                        ],
                        "isError": False,
                    },
                }
            except Exception as e:
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": f"sampling failed: {e}",
                            }
                        ],
                        "isError": True,
                    },
                }
        elif tool_name == "fail":
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": "failed intentionally",
                        }
                    ],
                    "isError": True,
                },
            }
        elif tool_name == "slow":
            time.sleep(3)
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": "slow done",
                        }
                    ],
                    "isError": False,
                },
            }
        elif tool_name == "crash_once":
            import os
            flag_file = args.get("flag_file")
            if flag_file and os.path.exists(flag_file):
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": "recovered after restart",
                            }
                        ],
                        "isError": False,
                    },
                }
            else:
                if flag_file:
                    with open(flag_file, "w") as f:
                        f.write("1")
                sys.exit(1)
        elif tool_name == "test_server_request":
            req_msg = {
                "jsonrpc": "2.0",
                "id": 9999,
                "method": "custom/request",
                "params": {},
            }
            sys.stdout.write(json.dumps(req_msg) + "\n")
            sys.stdout.flush()
            reply_line = sys.stdin.readline()
            try:
                reply = json.loads(reply_line)
                code = reply.get("error", {}).get("code")
                if code == -32601:
                    return {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": "server request handled successfully",
                                }
                            ],
                            "isError": False,
                        },
                    }
            except Exception:
                pass
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": "failed to get -32601 error",
                        }
                    ],
                    "isError": True,
                },
            }
        else:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": f"unknown tool: {tool_name}",
                        }
                    ],
                    "isError": True,
                },
            }

    return {
        "jsonrpc": "2.0",
        "id": req_id,
        "error": {
            "code": -32601,
            "message": f"Method not found: {method}",
        },
    }

def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception:
            continue

        if "id" not in msg or msg["id"] is None:
            # Notification: ignore
            continue

        resp = handle_request(msg)
        if resp is not None:
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()

if __name__ == "__main__":
    main()
