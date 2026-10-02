package com.example.fakesvc.controller;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Component;
import org.springframework.web.socket.CloseStatus;
import org.springframework.web.socket.TextMessage;
import org.springframework.web.socket.WebSocketSession;
import org.springframework.web.socket.handler.TextWebSocketHandler;

/**
 * WebSocket Welcome — /ws 入口，提示可用子路径。
 */
@Component
public class WsWelcomeHandler extends TextWebSocketHandler {

    private static final Logger log = LoggerFactory.getLogger(WsWelcomeHandler.class);

    @Override
    public void afterConnectionEstablished(WebSocketSession session) {
        log.info("WS Welcome connected: {} (uri={})", session.getId(), session.getUri());
        try {
            session.sendMessage(new TextMessage(
                "{\"welcome\":\"WebSocket 隧道已建立\",\"endpoints\":[\"/ws/echo\",\"/ws/clock\",\"/ws/chat\"]}"
            ));
        } catch (Exception e) {
            log.warn("Failed to send welcome message: {}", e.getMessage());
        }
    }

    @Override
    protected void handleTextMessage(WebSocketSession session, TextMessage message) throws Exception {
        String payload = message.getPayload();
        log.debug("WS Welcome received: {}", payload);
        // 回显消息
        session.sendMessage(new TextMessage("{\"echo\":" + payload + "}"));
    }

    @Override
    public void afterConnectionClosed(WebSocketSession session, CloseStatus status) {
        log.info("WS Welcome disconnected: {} status={}", session.getId(), status);
    }
}
