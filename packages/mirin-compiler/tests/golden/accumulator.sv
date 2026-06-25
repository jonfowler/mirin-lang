module accumulator (
    input  logic clk,
    input  logic rstn,
    input  logic [7:0] data,
    output logic [7:0] result
);
    logic [7:0] acc;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            acc <= 0;
        end else begin
            acc <= (acc + data);
        end
    end
    assign result = acc;
endmodule
