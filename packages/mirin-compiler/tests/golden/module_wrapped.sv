module multAdd (
    input  logic clk,
    input  logic rstn,
    input  logic [7:0] c,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] mult;
    assign mult = (a * b);
    logic [7:0] mult_1;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            mult_1 <= 0;
        end else begin
            mult_1 <= mult;
        end
    end
    logic [7:0] add;
    assign add = (mult_1 + c);
    assign result = add;
endmodule
