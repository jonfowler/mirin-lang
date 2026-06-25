module Option__reg (
    input  logic clk,
    input  logic self__valid,
    input  logic [7:0] self__payload,
    input  logic rstn,
    output logic result__valid,
    output logic [7:0] result__payload
);
    logic [7:0] payloadd;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            payloadd <= 0;
        end else begin
            payloadd <= self__payload;
        end
    end
    assign result__valid = self__valid;
    assign result__payload = payloadd;
endmodule

module double_delay (
    input  logic clk,
    input  logic rstn,
    input  logic upstream__valid,
    input  logic [7:0] upstream__payload,
    output logic downstream__valid,
    output logic [7:0] downstream__payload
);
    logic __call_0__valid;
    logic [7:0] __call_0__payload;
    Option__reg Option__reg (
        .clk(clk),
        .self__valid(upstream__valid),
        .self__payload(upstream__payload),
        .rstn(rstn),
        .result__valid(__call_0__valid),
        .result__payload(__call_0__payload)
    );
    Option__reg Option__reg_1 (
        .clk(clk),
        .self__valid(__call_0__valid),
        .self__payload(__call_0__payload),
        .rstn(rstn),
        .result__valid(downstream__valid),
        .result__payload(downstream__payload)
    );
endmodule
