module s (
    input  logic clk,
    input  logic rstn,
    input  logic signed [7:0] a,
    input  logic signed [7:0] b,
    output logic signed [7:0] result
);
    logic signed [7:0] d;
    assign d = (a - b);
    logic signed [7:0] n;
    assign n = (-d);
    logic signed [7:0] lo;
    logic signed [7:0] __block_0;
    always_comb begin
        if ((n < a)) begin
            __block_0 = n;
        end else begin
            __block_0 = a;
        end
    end
    assign lo = __block_0;
    logic signed [3:0] x;
    assign x = -8;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            result <= -1;
        end else begin
            result <= lo;
        end
    end
endmodule
