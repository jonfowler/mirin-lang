module registerPacket (
    input  logic clk,
    input  logic rstn,
    input  logic inp__valid,
    input  logic [7:0] inp__payload,
    output logic result__valid,
    output logic [7:0] result__payload
);
    logic held__valid;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__valid <= 1'b0;
        end else begin
            held__valid <= inp__valid;
        end
    end
    logic [7:0] held__payload;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__payload <= 0;
        end else begin
            held__payload <= inp__payload;
        end
    end
    assign result__valid = held__valid;
    assign result__payload = held__payload;
endmodule
